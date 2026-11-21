//! The picture pipeline: how the frames a game draws become the images
//! the window shows.
//!
//! Each place a screen is drawn has an image, keyed by what it shows
//! ([`ImageKey`]). [`Session::take_frames`] takes the emulation thread's
//! newest frames, and [`Session::refresh_images`] brings the images up to
//! date with them: colour corrected, turned, then drawn at the place's
//! size on the GPU ([`Upscaler`]) or filtered and enlarged on the CPU
//! ([`cpu_filter`]). The GPU is not waited for long: what it has not
//! finished stays on it, the images shown stay, and a later draw puts
//! its screens in place. [`Session::refresh_ambient`] keeps the backdrop
//! around the screens ([`Glow`]), and [`Session::keep_speed`] holds the 3D
//! resolution down while the game cannot keep its speed ([`Keeping`]).
//!
//! None of it reads the window's own state: the placements, the picture
//! and the rest are handed to it.

// A part of the view, split off: it shares the view's names.
use super::*;

/// What an image shows: a screen of a frame, turned, in a look, for a
/// place on the window so many device pixels across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ImageKey {
    screen: usize,
    rotation: Rotation,
    look: Look,
    /// The placement's size in device pixels.
    target: (u32, u32),
    /// Output size after sharing the GPU pixel budget across placements.
    drawn: (u32, u32),
    /// The last frame number on which this screen changed.
    sequence: u64,
}

impl ImageKey {
    /// A key no placement wants, for an image shown only until one drawn
    /// as now replaces it.
    pub(super) fn stale(self) -> Self {
        Self {
            target: (0, 0),
            ..self
        }
    }

    /// Whether an image for `self` may stand in for one for `wanted`
    /// until that is drawn: the same screen, look and size, from a frame
    /// no older than `shown`'s, the image there now, if any.
    fn stands_in(self, wanted: Self, shown: Option<Self>) -> bool {
        let same = |key: Self| {
            Self {
                sequence: wanted.sequence,
                ..key
            } == wanted
        };
        same(self) && shown.is_none_or(|shown| !same(shown) || shown.sequence < self.sequence)
    }
}

/// A screen sent to the upscaler: where it goes, what it shows, and its
/// size in device pixels.
#[derive(Debug, Clone, Copy)]
pub(super) struct Upscaling {
    index: usize,
    key: ImageKey,
    size: (u32, u32),
}

/// Match Display's check that the game keeps the speed asked for
/// ([`picture::SpeedKeeper`]).
pub(super) struct Keeping {
    keeper: picture::SpeedKeeper,
    set_for: Option<(u32, Speed)>,
    pub(super) measured: Option<(Instant, u64)>,
    /// The scale just changed: the first second after is not measured, as
    /// the 3DS's core rebuilds what it draws into, a hitch of its own.
    pub(super) settling: bool,
}

impl Keeping {
    /// Starting from `most`, the scale that matches the screens.
    pub(super) fn new(most: u32) -> Self {
        Self {
            keeper: picture::SpeedKeeper::new(most),
            set_for: None,
            measured: None,
            settling: false,
        }
    }
}

impl Session {
    /// The scale Match Display draws 3D at: `matched`, the scale that
    /// matches the screens, or less while the game cannot keep `speed`,
    /// measured every [`picture::SPEED_CHECK`].
    pub(super) fn keep_speed(&mut self, matched: u32, speed: Speed) -> u32 {
        let keeping = &mut self.keeping;
        if keeping.set_for != Some((matched, speed)) {
            *keeping = Keeping::new(matched);
            keeping.set_for = Some((matched, speed));
        }
        let now = Instant::now();
        let frames = self.emulator.frames_shown();
        match keeping.measured {
            Some((at, before)) if now.duration_since(at) >= picture::SPEED_CHECK => {
                // Frame counts are far below 2^52, so f64 holds them exactly.
                let ran =
                    frames.saturating_sub(before) as f64 / now.duration_since(at).as_secs_f64();
                let normal = self.info.frame_rate.hz();
                let wanted = match speed {
                    Speed::Unlimited => None,
                    Speed::Times(rate) => Some(normal * rate.get()),
                };
                if !std::mem::take(&mut keeping.settling) {
                    keeping.keeper.check(self.render_scale, ran, wanted, normal);
                }
                keeping.measured = Some((now, frames));
            }
            Some(_) => {}
            None => keeping.measured = Some((now, frames)),
        }
        matched.min(keeping.keeper.ceiling())
    }
}

/// The GPU half of the upscaler ([`gpu_upscale`](crate::gpu_upscale)).
pub(super) enum Upscaler {
    /// Not started: no picture has needed it.
    Idle,
    /// Starting on a background thread ([`EmulatorView::start_upscaler`]).
    Starting,
    Ready(Gpu),
    /// It would not start, or failed: screens stay on the CPU path, with
    /// Scale2x and Scale3x standing in for Rounded Pixels. Holds why
    /// until the player has been told.
    Off(Option<GpuError>),
}

/// What became of screens sent to the upscaler.
enum Sent {
    /// They are on the GPU.
    Yes,
    /// The GPU is starting or off: the CPU draws them.
    Unavailable,
    /// The last batch is still on the GPU: the screens shown stay until it
    /// is taken, and these are sent with the next frame.
    Busy,
}

/// How long a draw waits for the upscaler before showing what it showed
/// last and taking the new screens once they are done. On an M5 Max a
/// frame's screens usually take 0.5–2.5 ms; with other apps busy on the
/// GPU, one in a hundred took 14–30 ms, and waiting that out made the
/// window miss refreshes.
const UPSCALE_WAIT: Duration = Duration::from_millis(4);

impl Upscaler {
    /// Sends `requests` to the GPU, to be taken with [`Upscaler::take`].
    ///
    /// # Errors
    ///
    /// The [`GpuError`] that turned it off, once.
    fn send(&mut self, requests: &[Request<'_>]) -> Result<Sent, GpuError> {
        match self {
            Self::Ready(gpu) => match gpu.submit(requests) {
                Ok(true) => Ok(Sent::Yes),
                Ok(false) => Ok(Sent::Busy),
                Err(error) => {
                    *self = Self::Off(None);
                    Err(error)
                }
            },
            Self::Off(untold) => untold.take().map_or(Ok(Sent::Unavailable), Err),
            Self::Idle | Self::Starting => Ok(Sent::Unavailable),
        }
    }

    /// The batch on the GPU's screens, as BGRA bytes in the order sent, if
    /// it finishes within `wait`.
    ///
    /// # Errors
    ///
    /// The [`GpuError`] that turned it off.
    fn take(&mut self, wait: Duration) -> Option<Result<Vec<Bgra>, GpuError>> {
        let Self::Ready(gpu) = self else {
            return None;
        };
        let taken = gpu.finish(Some(wait))?;
        if taken.is_err() {
            *self = Self::Off(None);
        }
        Some(taken)
    }

    /// Whether a batch is on the GPU.
    pub(super) fn busy(&self) -> bool {
        matches!(self, Self::Ready(gpu) if gpu.busy())
    }

    /// Whether the batch on the GPU is done, to take without waiting.
    pub(super) fn finished(&self) -> bool {
        matches!(self, Self::Ready(gpu) if gpu.finished())
    }

    fn is_off(&self) -> bool {
        matches!(self, Self::Off(_))
    }
}

/// The CPU pass that draws `look` while the GPU is starting, or without
/// one: sharp pixels enlarged by a whole factor, the shading filters, and
/// without the GPU, Scale2x and Scale3x standing in for Rounded Pixels, as
/// large as `factor` device pixels to a console pixel allows.
fn cpu_filter(look: Look, factor: usize, gpu_off: bool) -> Filter {
    match look {
        Look::LcdGrid => Filter::LcdGrid,
        Look::Scanlines => Filter::Scanlines,
        Look::Upscaled(Method::Rounded) if gpu_off && factor >= 3 => Filter::Scale3x,
        Look::Upscaled(Method::Rounded) if gpu_off => Filter::Scale2x,
        Look::Upscaled(_) => Filter::None,
    }
}

/// The ambient backdrop: what it was last sampled from, its fade toward
/// that, and the image of where the fade has got to.
pub(super) struct Glow {
    /// The placements and the newest change to a screen they show.
    sampled: (Vec<Placement>, u64),
    pub(super) fade: ambient::Fade,
    pub(super) image: Arc<RenderImage>,
}

/// Records the frame number of each changed screen. A new frame still
/// reaches the timing lock even if every image can be reused.
fn note_screen_changes(sequences: &mut Vec<u64>, before: &[Frame], after: &[Frame], sequence: u64) {
    if before.len() != after.len() {
        // Adding or removing a screen can change the combined backdrop too.
        sequences.clear();
        sequences.resize(after.len(), sequence);
        return;
    }
    for ((stamp, previous), current) in sequences.iter_mut().zip(before).zip(after) {
        if previous != current {
            *stamp = sequence;
        }
    }
}

impl Session {
    /// Takes the emulation thread's latest frames, if they are new. The
    /// screen images drawn from the last ones stay until images of these
    /// replace them ([`Session::refresh_images`]), and the ambient
    /// backdrop fades over to them.
    pub(super) fn take_frames(&mut self) {
        let Some(sequence) = self
            .emulator
            .take_latest(self.sequence, &mut self.incoming_frames)
        else {
            return;
        };
        note_screen_changes(
            &mut self.screen_sequences,
            &self.frames,
            &self.incoming_frames,
            sequence,
        );
        std::mem::swap(&mut self.frames, &mut self.incoming_frames);
        self.sequence = sequence;
        self.taken_at = Some(Instant::now());
    }

    /// Puts the upscaler's finished screens, `taken`, where they were
    /// sent for, wherever each still stands in for what `wanted` there
    /// ([`ImageKey::stands_in`]). Returns the GPU's error if it failed.
    fn place_upscaled(
        &mut self,
        taken: Option<Result<Vec<Bgra>, GpuError>>,
        wanted: &[Option<ImageKey>],
        window: &mut Window,
    ) -> Option<GpuError> {
        let images = match taken? {
            Ok(images) => images,
            Err(error) => {
                self.upscaling.clear();
                return Some(error);
            }
        };
        // A batch sent for another game, or before the screens were
        // rebuilt, has nothing here to stand in for.
        for (sent, bgra) in std::mem::take(&mut self.upscaling).into_iter().zip(images) {
            let Some(Some(wanted)) = wanted.get(sent.index) else {
                continue;
            };
            let slot = &mut self.images[sent.index];
            if !sent
                .key
                .stands_in(*wanted, slot.as_ref().map(|(key, _)| *key))
            {
                continue;
            }
            let image = video::bgra_image(sent.size.0, sent.size.1, bgra);
            if let Some((_, old)) = slot.replace((sent.key, image)) {
                window.drop_image(old).ok();
            }
        }
        None
    }

    /// Brings the images up to date with how each placement draws its
    /// screen. Each frame is colour corrected and turned, then upscaled on
    /// the GPU to the placement's exact size in device pixels, or filtered
    /// and enlarged by a whole factor on the CPU.
    ///
    /// The GPU is waited for up to [`UPSCALE_WAIT`]. A batch that takes
    /// longer stays on it, the images shown stay, and a later draw puts
    /// it in place; meanwhile newer frames wait rather than queue.
    ///
    /// Returns the GPU's error the moment it fails, after which screens
    /// stay on the CPU, and whether a batch is still on the GPU, for the
    /// view to draw again when it is done.
    pub(super) fn refresh_images(
        &mut self,
        placements: &[Placement],
        scale_factor: f32,
        color_correction: bool,
        picture: Picture,
        upscaler: &mut Upscaler,
        window: &mut Window,
    ) -> (Option<GpuError>, bool) {
        // One image per placement, so the layout decides how many.
        if self.images.len() > placements.len() {
            for (_, image) in self.images.drain(placements.len()..).flatten() {
                window.drop_image(image).ok();
            }
        }
        self.images.resize(placements.len(), None);
        let look = picture.look(self.info.system);
        let targets: Vec<(u32, u32)> = placements
            .iter()
            .map(|placement| {
                picture::device_size(placement.rect.width, placement.rect.height, scale_factor)
            })
            .collect();
        let drawn = picture::within_budget(&targets, picture::MAX_UPSCALED_PIXELS);
        // What each placement should show now.
        let wanted: Vec<Option<ImageKey>> = placements
            .iter()
            .zip(targets.iter().zip(&drawn))
            .map(|(placement, (&target, &drawn))| {
                self.screen_sequences
                    .get(placement.screen)
                    .map(|&sequence| ImageKey {
                        screen: placement.screen,
                        rotation: placement.rotation,
                        look,
                        target,
                        drawn,
                        sequence,
                    })
            })
            .collect();
        // An earlier batch the GPU has finished meanwhile.
        let mut failed = self.place_upscaled(upscaler.take(Duration::ZERO), &wanted, window);
        let correction = self.correction.as_ref().filter(|_| color_correction);

        // The screens to draw again, each frame colour corrected and turned.
        let mut redraws = Vec::new();
        for (index, (slot, placement)) in self.images.iter().zip(placements).enumerate() {
            let (Some(frame), Some(key)) = (self.frames.get(placement.screen), wanted[index])
            else {
                continue;
            };
            if slot.as_ref().is_some_and(|(built, _)| *built == key) {
                continue;
            }
            // Frames drawn at a higher 3D resolution have several pixels
            // to each console pixel.
            let native = self.info.screens.get(placement.screen);
            let cell = native.map_or(1, |native| frame.size().width / native.width.max(1));
            let mut frame = Cow::Borrowed(frame);
            if let Some(correction) = correction {
                frame = Cow::Owned(correction.frame(&frame));
            }
            if key.rotation != Rotation::Upright {
                frame = Cow::Owned(video::turn(&frame, key.rotation));
            }
            redraws.push((index, key, frame, cell));
        }

        // Drawn together on the GPU, each at its exact size. Sharp Pixels
        // draws any frame at any size. Smooth and Rounded Pixels only
        // enlarge: Smooth the frame itself, unless it is about as large
        // already, when it is shown as it is; Rounded the console's pixels,
        // which on a frame with larger 3D it works on beside the 3D; where
        // they would not enlarge, Sharp Pixels draws the frame instead.
        let requests: Vec<(usize, Request<'_>)> = redraws
            .iter()
            .enumerate()
            .filter_map(|(at, (_, key, frame, cell))| {
                let Look::Upscaled(method) = key.look else {
                    return None;
                };
                let shown = frame.size();
                let cell = (*cell).max(1);
                let (width, height) = key.drawn;
                let enlarges = |(across, down): (u16, u16)| {
                    width > u32::from(across) && height > u32::from(down)
                };
                if method == Method::Smooth
                    && picture::shown_as_drawn((shown.width, shown.height), (width, height))
                {
                    // Shown as it is, without a trip to the GPU.
                    return None;
                }
                let method = match method {
                    Method::Rounded if !enlarges((shown.width / cell, shown.height / cell)) => {
                        Method::Sharp
                    }
                    Method::Smooth if !enlarges((shown.width, shown.height)) => Method::Sharp,
                    method => method,
                };
                Some((
                    at,
                    Request {
                        frame,
                        method,
                        cell: u32::from(cell),
                        width,
                        height,
                    },
                ))
            })
            .collect();
        let gpu_requests: Vec<Request<'_>> = requests.iter().map(|(_, request)| *request).collect();
        // Whether each redraw is the GPU's: sent, or waiting for the batch
        // before it.
        let mut on_gpu = vec![false; redraws.len()];
        let mut sent = false;
        if !gpu_requests.is_empty() {
            match upscaler.send(&gpu_requests) {
                Ok(Sent::Yes) => {
                    sent = true;
                    self.upscaling = requests
                        .iter()
                        .map(|(at, request)| Upscaling {
                            index: redraws[*at].0,
                            key: redraws[*at].1,
                            size: (request.width, request.height),
                        })
                        .collect();
                    for (at, _) in &requests {
                        on_gpu[*at] = true;
                    }
                }
                Ok(Sent::Busy) => {
                    for (at, _) in &requests {
                        on_gpu[*at] = true;
                    }
                }
                Ok(Sent::Unavailable) => {}
                Err(error) => failed = Some(error),
            }
        }

        // The rest on the CPU: filtered, then enlarged by a whole factor.
        // A place the GPU is drawing for that shows nothing yet, as one the
        // layout just added, gets the CPU's picture meanwhile, under a key
        // the GPU's replaces.
        for ((index, key, frame, cell), on_gpu) in redraws.into_iter().zip(on_gpu) {
            if on_gpu && self.images[index].is_some() {
                continue;
            }
            let width = frame.size().width;
            let factor = video::prescale_factor(width, placements[index].rect.width, scale_factor);
            let filter = cpu_filter(key.look, factor, upscaler.is_off());
            let (filtered, factor) = filter.apply(&frame, factor, usize::from(cell), key.rotation);
            let image = video::render_image(&filtered, factor);
            let key = if on_gpu { key.stale() } else { key };
            if let Some((_, old)) = self.images[index].replace((key, image)) {
                window.drop_image(old).ok();
            }
        }

        // The batch just sent, if the GPU finishes it soon enough. A batch
        // from before is not waited for: once it is done, the view draws
        // again and sends these screens then.
        if sent {
            let taken = upscaler.take(UPSCALE_WAIT);
            failed = failed.or(self.place_upscaled(taken, &wanted, window));
        }
        (failed, upscaler.busy())
    }

    /// Brings the ambient backdrop up to date with the screens as
    /// `placements` show them, fading toward their colours. Whether it is
    /// still fading.
    pub(super) fn refresh_ambient(
        &mut self,
        placements: &[Placement],
        color_correction: bool,
        window: &mut Window,
    ) -> bool {
        let now = Instant::now();
        let sequence = placements
            .iter()
            .filter_map(|placement| self.screen_sequences.get(placement.screen))
            .max()
            .copied()
            .unwrap_or(0);
        let current = self
            .ambient
            .as_ref()
            .is_some_and(|glow| glow.sampled.1 == sequence && glow.sampled.0 == placements);
        if !current {
            let correction = self.correction.as_ref().filter(|_| color_correction);
            let Some(target) = ambient::picture(&self.frames, placements, correction) else {
                if let Some(glow) = self.ambient.take() {
                    window.drop_image(glow.image).ok();
                }
                return false;
            };
            let sampled = (placements.to_vec(), sequence);
            let Some(glow) = &mut self.ambient else {
                let fade = ambient::Fade::new(target, now);
                let image = video::render_image(&fade.shown().frame(ambient::BORDER), 1);
                self.ambient = Some(Glow {
                    sampled,
                    fade,
                    image,
                });
                return false;
            };
            glow.sampled = sampled;
            glow.fade.aim(target);
        }
        let Some(glow) = &mut self.ambient else {
            return false;
        };
        let changed = !current || !glow.fade.settled();
        let moving = glow.fade.step(now);
        if changed {
            let image = video::render_image(&glow.fade.shown().frame(ambient::BORDER), 1);
            window
                .drop_image(std::mem::replace(&mut glow.image, image))
                .ok();
        }
        moving
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changed_screens_get_new_image_keys() {
        let screen = Frame::new(ScreenSize::new(3, 2));
        let first = [screen.clone(), screen];
        let mut stamps = Vec::new();
        note_screen_changes(&mut stamps, &[], &first, 1);
        let mut second = first.clone();
        second[1].pixels_mut()[5] ^= 1;
        note_screen_changes(&mut stamps, &first, &second, 3);
        assert_eq!(stamps, [1, 3]);
        let mut third = second.clone();
        third[0] = Frame::new(ScreenSize::new(2, 3));
        note_screen_changes(&mut stamps, &second, &third, 4);
        assert_eq!(stamps, [4, 3]);
        note_screen_changes(&mut stamps, &third, &third, 5);
        assert_eq!(stamps, [4, 3]);
        note_screen_changes(&mut stamps, &third, &third[..1], 6);
        assert_eq!(stamps, [6]);
    }

    #[test]
    fn an_older_frame_stands_in_until_the_new_one_is_drawn() {
        let key = |sequence, target| ImageKey {
            screen: 0,
            rotation: Rotation::Upright,
            look: Look::Upscaled(Method::Sharp),
            target,
            drawn: target,
            sequence,
        };
        let wanted = key(10, (1309, 982));
        // The GPU finished frame 9 while frame 10 came: it replaces frame
        // 8, or an empty place, but not frame 10 or a newer 9.
        assert!(key(9, (1309, 982)).stands_in(wanted, Some(key(8, (1309, 982)))));
        assert!(key(9, (1309, 982)).stands_in(wanted, None));
        assert!(!key(9, (1309, 982)).stands_in(wanted, Some(key(10, (1309, 982)))));
        assert!(!key(9, (1309, 982)).stands_in(wanted, Some(key(9, (1309, 982)))));
        // Drawn for another size, look or screen: it never stands in, but
        // replaces whatever of those is shown.
        assert!(!key(10, (800, 600)).stands_in(wanted, None));
        let smooth = ImageKey {
            look: Look::Upscaled(Method::Smooth),
            ..wanted
        };
        assert!(!smooth.stands_in(wanted, None));
        let smaller_budget = ImageKey {
            drawn: (800, 600),
            ..wanted
        };
        assert!(!smaller_budget.stands_in(wanted, None));
        assert!(key(9, (1309, 982)).stands_in(wanted, Some(key(12, (800, 600)))));
    }
}
