//! Pictures in answers (decision 0015): a paragraph of `wiki:` images
//! drawn as pictures from the game's wiki, fetched on a worker thread the
//! first time their row is drawn.
//!
//! One picture fills the column, up to [`TALLEST`]; several sit side by
//! side on one baseline, each captioned. Artwork cut out on transparency
//! is drawn bare; maps and screens get rounded corners and a hairline.
//! Small sprites are scaled up by whole pixels before they reach the GPU,
//! so pixel art stays crisp. A picture the wiki doesn't have leaves no
//! gap.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use emu_guide::markdown::{Block, Picture};
use emu_guide::pictures::{self, PictureStore, Wiki};
use gpui::{
    AnyElement, Context, ElementId, ObjectFit, RenderImage, div, img, prelude::*, px, relative,
};

use super::answer::Drawing;
use super::palette::hsla;
use super::{Enlarged, GuidePanel, Row};
use crate::video;

/// The tallest a picture alone is drawn, in points.
const TALLEST: f32 = 240.0;
/// The tallest pictures side by side are drawn, by how many share a row.
const ROW_TALLEST: [f32; 3] = [150.0, 120.0, 96.0];
/// The most pictures in one row; more wrap.
const PER_ROW: usize = 4;
/// The space between pictures.
const GAP: f32 = 8.0;
/// How much bigger than its pixels a picture may be drawn.
const MOST_ENLARGED: f32 = 3.0;
/// Pictures whose longer side is at most this are scaled up by whole
/// pixels before drawing, as sprites are.
const SPRITE: u32 = 320;
/// Pictures are shrunk to this on their longer side.
const LARGEST: u32 = 1600;
/// How long a picture takes to fade in.
const FADE: Duration = Duration::from_millis(220);
/// How far a list item or quote indents its blocks.
const INDENT: f32 = 26.0;

/// Where a picture is.
pub(super) enum Shown {
    /// On its way.
    Loading,
    /// Here.
    Ready(Ready),
    /// The wiki has none, or it didn't come.
    Failed,
}

/// A picture to draw.
pub(super) struct Ready {
    pub image: Arc<RenderImage>,
    /// Its size in pixels as fetched, before sprites are scaled up.
    width: u32,
    height: u32,
    /// Whether its corners are transparent: artwork cut out.
    cutout: bool,
    /// The wiki page it illustrates.
    pub page: String,
    arrived: Instant,
}

/// What the worker passes back.
struct Decoded {
    image: Arc<RenderImage>,
    width: u32,
    height: u32,
    cutout: bool,
    page: String,
}

/// Whether `block` or a block inside it shows pictures, finished or not.
fn has_pictures(block: &Block) -> bool {
    match block {
        Block::Pictures(_) => true,
        Block::List { items, .. } => items.iter().flatten().any(has_pictures),
        Block::Quote(blocks) => blocks.iter().any(has_pictures),
        _ => false,
    }
}

/// The finished picture addresses in `block` and the blocks inside it.
pub(super) fn targets(block: &Block) -> Vec<&str> {
    match block {
        Block::Pictures(pictures) => pictures
            .iter()
            .filter_map(|picture| picture.target.as_deref())
            .collect(),
        Block::List { items, .. } => items.iter().flatten().flat_map(targets).collect(),
        Block::Quote(blocks) => blocks.iter().flat_map(targets).collect(),
        _ => Vec::new(),
    }
}

impl GuidePanel {
    /// Starts fetching the pictures `block` shows that haven't been asked
    /// for yet.
    pub(super) fn request_pictures(&mut self, block: &Block, cx: &mut Context<Self>) {
        for target in targets(block) {
            if self.pictures.contains_key(target) {
                continue;
            }
            let target = target.to_owned();
            let Some(wiki) = self.wiki.clone() else {
                self.pictures.insert(target, Shown::Failed);
                continue;
            };
            if pictures::title(&target).is_none() {
                self.pictures.insert(target, Shown::Failed);
                continue;
            }
            self.pictures.insert(target.clone(), Shown::Loading);
            self.fetch_picture(target, wiki, cx);
        }
    }

    fn fetch_picture(&mut self, target: String, wiki: Wiki, cx: &mut Context<Self>) {
        let store = self.picture_store.clone();
        let generation = self.picture_generation;
        let (sender, received) = async_channel::bounded(1);
        let asked = target.clone();
        // curl blocks for seconds at worst: a thread of its own, as
        // answers get.
        let spawned = thread::Builder::new()
            .name("guide-picture".to_owned())
            .spawn(move || {
                let decoded = store
                    .fetch(&wiki, &asked)
                    .ok()
                    .and_then(|fetched| decode(&fetched.bytes, fetched.page));
                sender.send_blocking(decoded).ok();
            });
        if spawned.is_err() {
            self.pictures.insert(target, Shown::Failed);
            return;
        }
        cx.spawn(async move |this, cx| {
            let decoded = received.recv().await.ok().flatten();
            this.update(cx, |this, cx| {
                this.picture_arrived(target, generation, decoded, cx);
            })
            .ok();
        })
        .detach();
    }

    fn picture_arrived(
        &mut self,
        target: String,
        generation: u64,
        decoded: Option<Decoded>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.picture_generation {
            if let Some(decoded) = decoded {
                self.stale.push(decoded.image);
            }
            return;
        }
        let shown = match decoded {
            Some(decoded) => Shown::Ready(Ready {
                image: decoded.image,
                width: decoded.width,
                height: decoded.height,
                cutout: decoded.cutout,
                page: decoded.page,
                arrived: Instant::now(),
            }),
            None => Shown::Failed,
        };
        self.pictures.insert(target.clone(), shown);
        // The rows showing it are measured again.
        for (index, row) in self.rows.iter().enumerate() {
            let Row::Block(turn, block) = *row else {
                continue;
            };
            let shows = self
                .documents
                .get(turn)
                .and_then(|document| document.blocks().get(block))
                .is_some_and(|block| targets(block).contains(&target.as_str()));
            if shows {
                self.list.splice(index..index + 1, 1);
            }
        }
        self.follow();
        cx.notify();
    }

    /// Follows the transcript's width, which pictures are sized to. Called
    /// as the panel renders, outside the list, whose width is as it was
    /// last laid out: rows with pictures are measured again when it
    /// changes, and the next frame checks again.
    pub(super) fn follow_column(&mut self, window: &mut gpui::Window) {
        let width = f32::from(self.list.viewport_bounds().size.width);
        if width <= 0.0 || (width - self.column).abs() < 0.5 {
            return;
        }
        self.column = width;
        for (index, row) in self.rows.iter().enumerate() {
            let Row::Block(turn, block) = *row else {
                continue;
            };
            let pictured = self
                .documents
                .get(turn)
                .and_then(|document| document.blocks().get(block))
                .is_some_and(has_pictures);
            if pictured {
                self.list.splice(index..index + 1, 1);
            }
        }
        window.request_animation_frame();
    }

    /// Forgets every picture, as a chat or game changes.
    pub(super) fn forget_pictures(&mut self) {
        self.picture_generation += 1;
        for (_, shown) in self.pictures.drain() {
            if let Shown::Ready(ready) = shown {
                self.stale.push(ready.image);
            }
        }
    }

    /// Whether every picture in `block` is known to be missing, so its row
    /// is left out.
    pub(super) fn pictures_all_failed(&self, block: &Block) -> bool {
        let Block::Pictures(pictures) = block else {
            return false;
        };
        pictures.iter().all(|picture| {
            picture
                .target
                .as_deref()
                .is_some_and(|target| matches!(self.pictures.get(target), Some(Shown::Failed)))
        })
    }

    /// A paragraph of pictures; `path` says how deep in lists and quotes
    /// it is, which narrows it.
    pub(super) fn pictures_element(
        &self,
        pictures: &[Picture],
        drawing: &Drawing,
        path: &[usize],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let column = if self.column > 100.0 {
            self.column
        } else {
            super::COLUMN_WIDTH
        };
        let width =
            (column - 2.0 * super::render::SIDE - INDENT * path.len().saturating_sub(1) as f32)
                .max(120.0);
        // Those the wiki lacks leave no gap.
        let shown: Vec<&Picture> = pictures
            .iter()
            .filter(|picture| {
                !picture
                    .target
                    .as_deref()
                    .is_some_and(|target| matches!(self.pictures.get(target), Some(Shown::Failed)))
            })
            .collect();
        let id = |index: usize| {
            ElementId::Name(
                format!(
                    "guide-picture-{}-{}-{index}",
                    drawing.turn,
                    path.iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(".")
                )
                .into(),
            )
        };
        if let [picture] = shown[..] {
            return self
                .picture_cell(picture, id(0), (width, TALLEST), false, drawing, cx)
                .into_any_element();
        }
        let per_row = shown.len().clamp(1, PER_ROW);
        let cell = (width - GAP * (per_row - 1) as f32) / per_row as f32;
        let tallest = ROW_TALLEST[(per_row.saturating_sub(2)).min(2)];
        div()
            .flex()
            .flex_wrap()
            .items_end()
            .gap(px(GAP))
            .children(shown.iter().enumerate().map(|(index, picture)| {
                div().w(px(cell)).flex_none().child(self.picture_cell(
                    picture,
                    id(index),
                    (cell, tallest),
                    true,
                    drawing,
                    cx,
                ))
            }))
            .into_any_element()
    }

    /// One picture and its caption, fitting `bounds` in points. `centred`
    /// for pictures in a row.
    fn picture_cell(
        &self,
        picture: &Picture,
        id: ElementId,
        (most_width, most_height): (f32, f32),
        centred: bool,
        drawing: &Drawing,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let ink = drawing.ink;
        let ready = picture
            .target
            .as_deref()
            .and_then(|target| match self.pictures.get(target) {
                Some(Shown::Ready(ready)) => Some(ready),
                _ => None,
            });
        let caption = div()
            .w_full()
            .pt(px(4.0))
            .text_size(px(12.0))
            .line_height(px(16.0))
            .text_color(ink.muted)
            .truncate()
            .when(centred, |caption| caption.text_center())
            .child(picture.alt.clone());
        let Some(ready) = ready else {
            // Its size is known only once it comes.
            let height = if centred { most_height } else { 140.0 };
            return div()
                .flex()
                .flex_col()
                .w_full()
                .child(
                    div()
                        .w(if centred {
                            relative(1.0)
                        } else {
                            relative(0.62)
                        })
                        .h(px(height))
                        .rounded(px(12.0))
                        .bg(ink.raised),
                )
                .child(caption);
        };
        let (width, height) = fit(
            (ready.width as f32, ready.height as f32),
            (most_width, most_height),
        );
        let opacity = if self.reduce_motion {
            1.0
        } else {
            (ready.arrived.elapsed().as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
        };
        let image = ready.image.clone();
        let enlarged = Enlarged {
            image: image.clone(),
            caption: Some((
                picture.alt.clone(),
                self.wiki
                    .as_ref()
                    .map(|wiki| wiki.name.clone())
                    .unwrap_or_default(),
                ready.page.clone(),
            )),
        };
        let mut drawn = img(image)
            .w(px(width))
            .h(px(height))
            .max_w_full()
            .object_fit(ObjectFit::Contain)
            .opacity(1.0 - (1.0 - opacity).powi(2));
        if !ready.cutout {
            drawn = drawn
                .rounded(px(10.0))
                .border_1()
                .border_color(hsla([1.0; 3], 0.08));
        }
        div()
            .flex()
            .flex_col()
            .when(centred, |cell| cell.items_center())
            .w_full()
            .child(
                div()
                    .id(id)
                    .cursor_pointer()
                    .child(drawn)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.enlarged = Some(enlarged.clone());
                        cx.notify();
                    })),
            )
            .child(caption)
    }

    /// Whether a picture is still fading in.
    pub(super) fn pictures_fading(&self) -> bool {
        !self.reduce_motion
            && self
                .pictures
                .values()
                .any(|shown| matches!(shown, Shown::Ready(ready) if ready.arrived.elapsed() < FADE))
    }
}

/// `size` scaled to fit `bounds`, never more than [`MOST_ENLARGED`] times.
fn fit((width, height): (f32, f32), (most_width, most_height): (f32, f32)) -> (f32, f32) {
    if width <= 0.0 || height <= 0.0 {
        return (0.0, 0.0);
    }
    let scale = (most_width / width)
        .min(most_height / height)
        .min(MOST_ENLARGED);
    ((width * scale).round(), (height * scale).round())
}

/// A fetched picture as GPUI draws it, with its size and whether it is cut
/// out; `None` if it isn't one the app can read.
fn decode(bytes: &[u8], page: String) -> Option<Decoded> {
    let picture = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = picture.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let cutout = [
        (0, 0),
        (width - 1, 0),
        (0, height - 1),
        (width - 1, height - 1),
    ]
    .iter()
    .all(|&(x, y)| picture.get_pixel(x, y).0[3] < 16);
    let longer = width.max(height);
    let mut shown = if longer <= SPRITE {
        // Whole pixels, to about 640 on the longer side.
        let factor = (640 / longer).clamp(1, 8);
        image::imageops::resize(
            &picture,
            width * factor,
            height * factor,
            image::imageops::FilterType::Nearest,
        )
    } else if longer > LARGEST {
        image::imageops::resize(
            &picture,
            (width * LARGEST / longer).max(1),
            (height * LARGEST / longer).max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        picture
    };
    let (width_shown, height_shown) = shown.dimensions();
    // A sprite is sized by its own pixels; anything else as shrunk.
    let (width, height) = if longer <= SPRITE {
        (width, height)
    } else {
        (width_shown, height_shown)
    };
    // GPUI's images hold BGRA.
    for pixel in shown.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Decoded {
        image: video::bgra_image(width_shown, height_shown, shown.into_raw()),
        width,
        height,
        cutout,
        page,
    })
}

/// The cache folder pictures are kept in.
pub(super) fn store() -> PictureStore {
    PictureStore::new(
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("ds-emulator")
            .join("Guide pictures"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_fit_their_bounds_and_small_ones_grow_a_little() {
        // A map, wider than the column.
        assert_eq!(fit((800.0, 840.0), (320.0, 240.0)), (229.0, 240.0));
        // A landscape screen.
        assert_eq!(fit((800.0, 400.0), (320.0, 240.0)), (320.0, 160.0));
        // A sprite: three times, no more.
        assert_eq!(fit((40.0, 30.0), (320.0, 240.0)), (120.0, 90.0));
        assert_eq!(fit((0.0, 30.0), (320.0, 240.0)), (0.0, 0.0));
    }

    fn png(width: u32, height: u32, alpha: u8) -> Vec<u8> {
        let picture = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, alpha]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        picture
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn sprites_grow_by_whole_pixels_and_cutouts_are_seen() {
        let sprite = decode(&png(80, 40, 0), String::new()).unwrap();
        assert_eq!((sprite.width, sprite.height), (80, 40));
        assert!(sprite.cutout);
        assert_eq!(
            sprite.image.size(0),
            gpui::size(gpui::DevicePixels(640), gpui::DevicePixels(320))
        );
        let map = decode(&png(2000, 1000, 255), String::new()).unwrap();
        assert_eq!((map.width, map.height), (1600, 800));
        assert!(!map.cutout);
        let tall = decode(&png(1000, 3000, 255), String::new()).unwrap();
        assert_eq!((tall.width, tall.height), (533, 1600));
        assert!(decode(b"not a picture", String::new()).is_none());
    }
}
