//! A QuickTime movie writer: Photo-JPEG video and 16-bit stereo PCM sound,
//! which QuickTime Player opens on macOS as do VLC, IINA and ffmpeg.
//!
//! The file is written as frames arrive: an `ftyp` atom, then one `mdat`
//! atom holding each frame's JPEG followed by the sound that played during
//! it, and at the end a `moov` atom indexing them. A movie whose writer
//! never finished has no index and does not play.
//!
//! Layout follows Apple's QuickTime File Format specification. Sizes and
//! durations are big-endian; media durations are 64-bit (version 1
//! headers), so a track's time scale can be the console's exact frame rate.
//!
//! Sound is timed against the video: its rate in the file is the sound
//! frames written per second of video, so both end together even when the
//! source's sound clock is slightly off its nominal rate. (The DS mixes at
//! 33,513,982 / 1024 = 32,728.5 Hz and reports 32,768 Hz; played at that,
//! sound would drift four seconds an hour.)

use std::io::{self, Seek, SeekFrom, Write};
use std::time::Duration;

use emu_core::StereoSample;

/// Time units per second of the movie as a whole; tracks have their own.
const MOVIE_TIMESCALE: u32 = 1000;

/// The identity transform, in QuickTime's 16.16 and 2.30 fixed point.
const MATRIX: [u32; 9] = [0x1_0000, 0, 0, 0, 0x1_0000, 0, 0, 0, 0x4000_0000];

/// Bytes in one stereo 16-bit sound frame.
const SOUND_FRAME_BYTES: u32 = 4;

/// The video's shape and timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Video {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// Time units per second.
    pub timescale: u32,
    /// Time units per frame.
    pub frame_duration: u32,
}

/// Writes a movie to `out` frame by frame.
pub struct MovWriter<W: Write + Seek> {
    out: W,
    video: Video,
    /// Sound frames per second, as the source says.
    nominal_rate: u32,
    /// Where the `mdat` atom starts, for its size at the end.
    mdat_start: u64,
    /// Where the next byte goes.
    position: u64,
    /// Each video frame's JPEG: where it starts and its length.
    frames: Vec<(u64, u32)>,
    /// Each run of sound: where it starts and how many sound frames.
    sound: Vec<(u64, u32)>,
    /// Sound frames written so far.
    sound_frames: u64,
}

/// An atom's contents under construction.
#[derive(Default)]
struct Atom(Vec<u8>);

impl Atom {
    /// A "full" atom's contents, which start with a version and flags.
    fn full(version: u8, flags: u32) -> Self {
        Self(vec![version]).bytes(&flags.to_be_bytes()[1..])
    }

    fn bytes(mut self, bytes: &[u8]) -> Self {
        self.0.extend_from_slice(bytes);
        self
    }

    fn u16(self, value: u16) -> Self {
        self.bytes(&value.to_be_bytes())
    }

    fn u32(self, value: u32) -> Self {
        self.bytes(&value.to_be_bytes())
    }

    fn u64(self, value: u64) -> Self {
        self.bytes(&value.to_be_bytes())
    }

    fn matrix(self) -> Self {
        MATRIX.iter().fold(self, |atom, value| atom.u32(*value))
    }

    /// A Pascal string: its length, then its bytes.
    fn counted(self, text: &str) -> Self {
        let length = u8::try_from(text.len()).expect("names are short");
        self.bytes(&[length]).bytes(text.as_bytes())
    }

    /// The finished atom: size, `kind`, contents.
    fn kind(self, kind: &[u8; 4]) -> Vec<u8> {
        let size = u32::try_from(self.0.len() + 8).expect("index atoms stay under 4 GiB");
        [&size.to_be_bytes()[..], kind, &self.0].concat()
    }
}

/// `count` as a 32-bit table field, or an error once a recording outgrows
/// the format.
fn table_count(count: u64) -> io::Result<u32> {
    u32::try_from(count).map_err(|_| io::Error::other("the recording is too long for a movie"))
}

/// `duration` in `timescale` units, in the movie's time scale, rounded up
/// so that players keep the last frame and sound frame.
fn movie_time(duration: u64, timescale: u32) -> u32 {
    let scaled = u128::from(duration) * u128::from(MOVIE_TIMESCALE);
    u32::try_from(scaled.div_ceil(u128::from(timescale.max(1)))).unwrap_or(u32::MAX)
}

impl<W: Write + Seek> MovWriter<W> {
    /// Starts a movie with `video` frames and stereo sound at about
    /// `nominal_rate` frames per second.
    ///
    /// # Errors
    ///
    /// Returns the error from writing the file's first bytes.
    pub fn new(mut out: W, video: Video, nominal_rate: u32) -> io::Result<Self> {
        let ftyp = Atom::default()
            .bytes(b"qt  ")
            .u32(0x0000_0200)
            .bytes(b"qt  ")
            .kind(b"ftyp");
        out.write_all(&ftyp)?;
        let mdat_start = ftyp.len() as u64;
        // A 64-bit size, filled in when the movie is finished.
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&0u64.to_be_bytes())?;
        Ok(Self {
            out,
            video,
            nominal_rate,
            mdat_start,
            position: mdat_start + 16,
            frames: Vec::new(),
            sound: Vec::new(),
            sound_frames: 0,
        })
    }

    /// Adds one video frame, as a baseline JPEG with 4:2:0 chroma, and the
    /// sound that plays during it.
    ///
    /// # Errors
    ///
    /// Returns any write error, or an error if the movie would grow past
    /// what its index can count (about a day of sound).
    pub fn push(&mut self, jpeg: &[u8], sound: &[StereoSample]) -> io::Result<()> {
        table_count(self.frames.len() as u64 + 1)?;
        table_count(self.sound_frames + sound.len() as u64)?;
        let length = u32::try_from(jpeg.len()).map_err(|_| io::Error::other("frame too large"))?;
        self.frames.push((self.position, length));
        self.out.write_all(jpeg)?;
        self.position += u64::from(length);
        if !sound.is_empty() {
            let bytes: Vec<u8> = sound
                .iter()
                .flat_map(|[left, right]| {
                    let [l0, l1] = left.to_le_bytes();
                    let [r0, r1] = right.to_le_bytes();
                    [l0, l1, r0, r1]
                })
                .collect();
            self.sound
                .push((self.position, table_count(sound.len() as u64)?));
            self.out.write_all(&bytes)?;
            self.position += bytes.len() as u64;
            self.sound_frames += sound.len() as u64;
        }
        Ok(())
    }

    /// Video frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames.len() as u64
    }

    /// How long the video written so far plays for.
    pub fn duration(&self) -> Duration {
        let units = self.frames() * u64::from(self.video.frame_duration);
        let nanos = u128::from(units) * 1_000_000_000 / u128::from(self.video.timescale.max(1));
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// Writes the index and fills in the media's size, finishing the
    /// movie, and returns the output.
    ///
    /// # Errors
    ///
    /// Returns any write or seek error.
    pub fn finish(mut self) -> io::Result<W> {
        let moov = self.moov()?;
        self.out.write_all(&moov)?;
        let mdat_size = self.position - self.mdat_start;
        self.out.seek(SeekFrom::Start(self.mdat_start + 8))?;
        self.out.write_all(&mdat_size.to_be_bytes())?;
        self.out.seek(SeekFrom::End(0))?;
        self.out.flush()?;
        Ok(self.out)
    }

    /// The sound's rate in the file: as many sound frames per second as
    /// were written per second of video, or the nominal rate until there
    /// is video.
    fn sound_rate(&self) -> u32 {
        let units = u128::from(self.frames()) * u128::from(self.video.frame_duration);
        if units == 0 || self.sound_frames == 0 {
            return self.nominal_rate;
        }
        let per_second = u128::from(self.sound_frames) * u128::from(self.video.timescale);
        // The sound description holds a 16.16 rate.
        let rate = (per_second + units / 2) / units;
        u32::try_from(rate.clamp(1, 0xFFFF)).unwrap_or(self.nominal_rate)
    }

    fn moov(&self) -> io::Result<Vec<u8>> {
        let video_units = self.frames() * u64::from(self.video.frame_duration);
        let video_time = movie_time(video_units, self.video.timescale);
        let mut tracks = vec![self.video_track(video_units, video_time)?];
        let mut movie_duration = video_time;
        if !self.sound.is_empty() {
            let rate = self.sound_rate();
            let sound_time = movie_time(self.sound_frames, rate);
            movie_duration = movie_duration.max(sound_time);
            tracks.push(self.sound_track(rate, sound_time)?);
        }
        let next_track = u32::try_from(tracks.len()).unwrap_or(2) + 1;
        let mvhd = Atom::full(0, 0)
            .u32(0) // created
            .u32(0) // modified
            .u32(MOVIE_TIMESCALE)
            .u32(movie_duration)
            .u32(0x1_0000) // preferred rate 1.0
            .u16(0x0100) // preferred volume 1.0
            .bytes(&[0; 10])
            .matrix()
            .bytes(&[0; 24]) // preview, poster, selection and current times
            .u32(next_track)
            .kind(b"mvhd");
        Ok(tracks
            .into_iter()
            .fold(Atom::default().bytes(&mvhd), |moov, track| {
                moov.bytes(&track)
            })
            .kind(b"moov"))
    }

    /// A track header: enabled and in the movie.
    fn tkhd(id: u32, duration: u32, volume: u16, video: Option<Video>) -> Vec<u8> {
        let (width, height) = video.map_or((0, 0), |video| (video.width, video.height));
        Atom::full(0, 0x3)
            .u32(0) // created
            .u32(0) // modified
            .u32(id)
            .u32(0)
            .u32(duration)
            .bytes(&[0; 8])
            .u16(0) // layer
            .u16(0) // alternate group
            .u16(volume)
            .u16(0)
            .matrix()
            .u32(u32::from(width) << 16)
            .u32(u32::from(height) << 16)
            .kind(b"tkhd")
    }

    /// A media header with a 64-bit duration.
    fn mdhd(timescale: u32, duration: u64) -> Vec<u8> {
        Atom::full(1, 0)
            .u64(0) // created
            .u64(0) // modified
            .u32(timescale)
            .u64(duration)
            .u16(0) // language
            .u16(0) // quality
            .kind(b"mdhd")
    }

    fn hdlr(component: &[u8; 4], subtype: &[u8; 4], name: &str) -> Vec<u8> {
        Atom::full(0, 0)
            .bytes(component)
            .bytes(subtype)
            .bytes(&[0; 12]) // manufacturer, flags, flags mask
            .counted(name)
            .kind(b"hdlr")
    }

    /// The media's data is in this file.
    fn dinf() -> Vec<u8> {
        let here = Atom::full(0, 1).kind(b"alis");
        let dref = Atom::full(0, 0).u32(1).bytes(&here).kind(b"dref");
        Atom::default().bytes(&dref).kind(b"dinf")
    }

    fn minf(header: &[u8], stbl: &[u8]) -> Vec<u8> {
        Atom::default()
            .bytes(header)
            .bytes(&Self::hdlr(b"dhlr", b"alis", "DataHandler"))
            .bytes(&Self::dinf())
            .bytes(stbl)
            .kind(b"minf")
    }

    /// A chunk offset table.
    fn co64(offsets: impl ExactSizeIterator<Item = u64>) -> io::Result<Vec<u8>> {
        let atom = Atom::full(0, 0).u32(table_count(offsets.len() as u64)?);
        Ok(offsets.fold(atom, Atom::u64).kind(b"co64"))
    }

    fn video_track(&self, units: u64, time: u32) -> io::Result<Vec<u8>> {
        let video = self.video;
        let mut name = [0u8; 32];
        let label = b"Photo - JPEG";
        name[0] = label.len() as u8;
        name[1..=label.len()].copy_from_slice(label);
        let entry = Atom::default()
            .bytes(&[0; 6])
            .u16(1) // data reference
            .u16(0) // version
            .u16(0) // revision
            .u32(0) // vendor
            .u32(0) // temporal quality
            .u32(0x200) // spatial quality: normal
            .u16(video.width)
            .u16(video.height)
            .u32(72 << 16) // 72 dpi
            .u32(72 << 16)
            .u32(0) // data size
            .u16(1) // frames per sample
            .bytes(&name)
            .u16(24) // depth
            .u16(0xFFFF) // no colour table
            .kind(b"jpeg");
        let count = table_count(self.frames())?;
        let stsd = Atom::full(0, 0).u32(1).bytes(&entry).kind(b"stsd");
        let stts = Atom::full(0, 0)
            .u32(1)
            .u32(count)
            .u32(video.frame_duration)
            .kind(b"stts");
        // One frame per chunk.
        let stsc = Atom::full(0, 0).u32(1).u32(1).u32(1).u32(1).kind(b"stsc");
        let stsz = self
            .frames
            .iter()
            .fold(Atom::full(0, 0).u32(0).u32(count), |atom, (_, size)| {
                atom.u32(*size)
            })
            .kind(b"stsz");
        let co64 = Self::co64(self.frames.iter().map(|(offset, _)| *offset))?;
        let stbl = Atom::default()
            .bytes(&stsd)
            .bytes(&stts)
            .bytes(&stsc)
            .bytes(&stsz)
            .bytes(&co64)
            .kind(b"stbl");
        let vmhd = Atom::full(0, 1).bytes(&[0; 8]).kind(b"vmhd");
        let mdia = Atom::default()
            .bytes(&Self::mdhd(video.timescale, units))
            .bytes(&Self::hdlr(b"mhlr", b"vide", "VideoHandler"))
            .bytes(&Self::minf(&vmhd, &stbl))
            .kind(b"mdia");
        Ok(Atom::default()
            .bytes(&Self::tkhd(1, time, 0, Some(video)))
            .bytes(&mdia)
            .kind(b"trak"))
    }

    fn sound_track(&self, rate: u32, time: u32) -> io::Result<Vec<u8>> {
        // Little-endian signed 16-bit PCM, sound description version 0.
        let entry = Atom::default()
            .bytes(&[0; 6])
            .u16(1) // data reference
            .u16(0) // version
            .u16(0) // revision
            .u32(0) // vendor
            .u16(2) // channels
            .u16(16) // bits per sample
            .u16(0) // compression
            .u16(0) // packet size
            .u32(rate << 16)
            .kind(b"sowt");
        let count = table_count(self.sound_frames)?;
        let stsd = Atom::full(0, 0).u32(1).bytes(&entry).kind(b"stsd");
        let stts = Atom::full(0, 0).u32(1).u32(count).u32(1).kind(b"stts");
        // Chunks hold each frame's sound; list where their length changes.
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for (chunk, (_, frames)) in (1u32..).zip(&self.sound) {
            if runs.last().map(|(_, length)| length) != Some(frames) {
                runs.push((chunk, *frames));
            }
        }
        let stsc = runs
            .iter()
            .fold(
                Atom::full(0, 0).u32(table_count(runs.len() as u64)?),
                |atom, (first, frames)| atom.u32(*first).u32(*frames).u32(1),
            )
            .kind(b"stsc");
        let stsz = Atom::full(0, 0)
            .u32(SOUND_FRAME_BYTES)
            .u32(count)
            .kind(b"stsz");
        let co64 = Self::co64(self.sound.iter().map(|(offset, _)| *offset))?;
        let stbl = Atom::default()
            .bytes(&stsd)
            .bytes(&stts)
            .bytes(&stsc)
            .bytes(&stsz)
            .bytes(&co64)
            .kind(b"stbl");
        let smhd = Atom::full(0, 0).u32(0).kind(b"smhd");
        let mdia = Atom::default()
            .bytes(&Self::mdhd(rate, self.sound_frames))
            .bytes(&Self::hdlr(b"mhlr", b"soun", "SoundHandler"))
            .bytes(&Self::minf(&smhd, &stbl))
            .kind(b"mdia");
        Ok(Atom::default()
            .bytes(&Self::tkhd(2, time, 0x0100, None))
            .bytes(&mdia)
            .kind(b"trak"))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const VIDEO: Video = Video {
        width: 4,
        height: 2,
        timescale: 262_144,
        frame_duration: 4389,
    };

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
    }

    /// The atoms in `bytes`: each one's kind and contents.
    fn atoms(bytes: &[u8]) -> Vec<(&[u8], &[u8])> {
        let mut list = Vec::new();
        let mut rest = bytes;
        while rest.len() >= 8 {
            let (mut size, mut header) = (u32_at(rest, 0) as usize, 8);
            if size == 1 {
                size = u64::from_be_bytes(rest[8..16].try_into().expect("8 bytes")) as usize;
                header = 16;
            }
            list.push((&rest[4..8], &rest[header..size]));
            rest = &rest[size..];
        }
        list
    }

    /// The contents of the first atom along `path`, each inside the last.
    fn find<'a>(bytes: &'a [u8], path: &[&[u8; 4]]) -> &'a [u8] {
        path.iter().fold(bytes, |within, kind| {
            let found = atoms(within).into_iter().find(|(atom, _)| atom == kind);
            found
                .map(|(_, body)| body)
                .unwrap_or_else(|| panic!("no {} atom", String::from_utf8_lossy(&kind[..])))
        })
    }

    /// The sample table of the movie's `index`th track.
    fn sample_table(movie: &[u8], index: usize) -> &[u8] {
        let moov = find(movie, &[b"moov"]);
        let mut tracks = atoms(moov).into_iter().filter(|(kind, _)| kind == b"trak");
        let (_, trak) = tracks.nth(index).expect("the track");
        find(trak, &[b"mdia", b"minf", b"stbl"])
    }

    fn movie(frames: &[(&[u8], usize)]) -> Vec<u8> {
        let mut writer = MovWriter::new(Cursor::new(Vec::new()), VIDEO, 48_000).expect("start");
        for (jpeg, sound) in frames {
            writer.push(jpeg, &vec![[1, -1]; *sound]).expect("push");
        }
        writer.finish().expect("finish").into_inner()
    }

    #[test]
    fn frames_and_sound_interleave_in_the_media_atom() {
        let bytes = movie(&[(b"JPEG1", 2), (b"JPG2", 3)]);
        assert_eq!(&bytes[4..8], b"ftyp");
        let mdat = find(&bytes, &[b"mdat"]);
        let sound = |frames: usize| [1u8, 0, 0xFF, 0xFF].repeat(frames);
        let expected = [&b"JPEG1"[..], &sound(2), b"JPG2", &sound(3)].concat();
        assert_eq!(mdat, expected);
    }

    #[test]
    fn the_index_points_at_every_frame() {
        let bytes = movie(&[(b"JPEG1", 2), (b"JPG2", 3)]);
        let video = sample_table(&bytes, 0);
        let stsz = find(video, &[b"stsz"]);
        assert_eq!((u32_at(stsz, 4), u32_at(stsz, 8)), (0, 2), "two frames");
        assert_eq!((u32_at(stsz, 12), u32_at(stsz, 16)), (5, 4));
        let co64 = find(video, &[b"co64"]);
        for (index, jpeg) in [&b"JPEG1"[..], b"JPG2"].iter().enumerate() {
            let at = 8 + index * 8;
            let offset = u64::from_be_bytes(co64[at..at + 8].try_into().expect("8 bytes"));
            let offset = usize::try_from(offset).expect("small");
            assert_eq!(&bytes[offset..offset + jpeg.len()], *jpeg);
        }
        let stts = find(video, &[b"stts"]);
        assert_eq!((u32_at(stts, 8), u32_at(stts, 12)), (2, 4389));
    }

    #[test]
    fn sound_chunks_are_listed_where_their_length_changes() {
        let bytes = movie(&[
            (b"A", 800),
            (b"B", 800),
            (b"C", 801),
            (b"D", 0),
            (b"E", 801),
        ]);
        let stbl = sample_table(&bytes, 1);
        let stsc = find(stbl, &[b"stsc"]);
        // Two runs: chunks 1-2 of 800 frames, chunks 3-4 of 801. Frame D
        // had no sound and so no chunk.
        assert_eq!(u32_at(stsc, 4), 2);
        assert_eq!((u32_at(stsc, 8), u32_at(stsc, 12)), (1, 800));
        assert_eq!((u32_at(stsc, 20), u32_at(stsc, 24)), (3, 801));
        let stsz = find(stbl, &[b"stsz"]);
        assert_eq!((u32_at(stsz, 4), u32_at(stsz, 8)), (4, 3202));
        let sowt = find(stbl, &[b"stsd"]);
        assert_eq!(&sowt[12..16], b"sowt");
    }

    #[test]
    fn durations_round_up_to_keep_the_last_frame() {
        let bytes = movie(&[(&b"A"[..], 1); 3]);
        let mvhd = find(&bytes, &[b"moov", b"mvhd"]);
        // Three frames of 4389/262144 s are 50.23 ms.
        assert_eq!((u32_at(mvhd, 12), u32_at(mvhd, 16)), (1000, 51));
        let mut writer = MovWriter::new(Cursor::new(Vec::new()), VIDEO, 48_000).expect("start");
        for _ in 0..60 {
            writer.push(b"A", &[]).expect("push");
        }
        assert_eq!(writer.duration().as_millis(), 1004);
    }

    #[test]
    fn sound_is_timed_to_end_with_the_video() {
        // 547 sound frames to each frame at 59.7275 frames a second is
        // 32,671 a second, whatever rate the source claimed.
        let frames = vec![(&b"A"[..], 547); 120];
        let bytes = movie(&frames);
        let stbl = sample_table(&bytes, 1);
        let rate = u32_at(find(stbl, &[b"stsd"]), 8 + 8 + 24) >> 16;
        assert_eq!(rate, 32_671);
        let moov = find(&bytes, &[b"moov"]);
        let tracks: Vec<&[u8]> = atoms(moov)
            .into_iter()
            .filter(|(kind, _)| kind == b"trak")
            .map(|(_, trak)| trak)
            .collect();
        let mdhd = |trak| find(trak, &[b"mdia", b"mdhd"]);
        // Version 1: the time scale follows two 64-bit times.
        assert_eq!(u32_at(mdhd(tracks[1]), 20), 32_671);
        let tkhd_duration = |trak| u32_at(find(trak, &[b"tkhd"]), 20);
        assert_eq!(tkhd_duration(tracks[0]), tkhd_duration(tracks[1]));
    }

    #[test]
    fn a_silent_movie_has_only_video() {
        let bytes = movie(&[(b"A", 0)]);
        let moov = find(&bytes, &[b"moov"]);
        let tracks = atoms(moov)
            .iter()
            .filter(|(kind, _)| kind == b"trak")
            .count();
        assert_eq!(tracks, 1);
    }
}
