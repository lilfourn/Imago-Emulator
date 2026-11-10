//! What the two-player cores share: two consoles joined by a link cable
//! or within wireless range, run as one [`Core`](crate::Core).
//!
//! How the two run together (the slices of time each console gets, and
//! what crosses the link between them) is each system's own and stays in
//! its crate. The plumbing around that is the same for every system and
//! is here: mixing the two consoles' sound into one stream.

use crate::StereoSample;

/// What [`mix`] does with the samples one console made and the other did
/// not, when a frame leaves them with different counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uneven {
    /// The mix ends with the shorter stream; the longer one's last samples
    /// are left out. Linked Game Boys and Game Boy Advances do this.
    Dropped,
    /// The longer stream plays to its end against silence. Linked DSes do
    /// this: a console whose power is off makes no sound at all.
    AgainstSilence,
}

/// Appends two consoles' sound to `out` as one stream: each sample is the
/// mean of the two consoles', channel by channel, rounded towards zero.
pub fn mix(
    first: &[StereoSample],
    second: &[StereoSample],
    uneven: Uneven,
    out: &mut Vec<StereoSample>,
) {
    let mean = |a: i16, b: i16| ((i32::from(a) + i32::from(b)) / 2) as i16;
    let length = match uneven {
        Uneven::Dropped => first.len().min(second.len()),
        Uneven::AgainstSilence => first.len().max(second.len()),
    };
    out.extend((0..length).map(|index| {
        let a = first.get(index).copied().unwrap_or_default();
        let b = second.get(index).copied().unwrap_or_default();
        [mean(a[0], b[0]), mean(a[1], b[1])]
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mix_is_the_mean_of_both_consoles_rounded_towards_zero() {
        let mut out = vec![[7, 7]];
        mix(
            &[[100, -100], [i16::MAX, i16::MIN]],
            &[[50, -51], [i16::MAX, i16::MIN]],
            Uneven::Dropped,
            &mut out,
        );
        assert_eq!(out, [[7, 7], [75, -75], [i16::MAX, i16::MIN]]);
    }

    #[test]
    fn an_uneven_mix_drops_the_extra_samples_or_plays_them_against_silence() {
        let (long, short) = ([[10, 20], [30, 40], [-50, 61]], [[10, 20]]);
        for (first, second) in [(&long[..], &short[..]), (&short[..], &long[..])] {
            let mut dropped = Vec::new();
            mix(first, second, Uneven::Dropped, &mut dropped);
            assert_eq!(dropped, [[10, 20]]);

            let mut padded = Vec::new();
            mix(first, second, Uneven::AgainstSilence, &mut padded);
            assert_eq!(padded, [[10, 20], [15, 20], [-25, 30]]);
        }
    }
}
