//! The F12 host menu. It is drawn on the CPU into the window buffer after
//! scaling, at window resolution, so it never reaches the guest frame or a
//! recording. The menu model knows nothing about windows or threads: keys
//! go in, [`Action`]s come out, and the frontend applies them. The REC
//! indicator is drawn the same way.
use crate::scaler::Scaler;
use gp64::recording::clock;
use gp64_hw::vga::CP437_FONT_8X16;
use gp64_machine::modem::LineStatus;
use winit::keyboard::KeyCode;

const GLYPH_W: usize = 8;
const GLYPH_H: usize = 16;
/// Pixels kept clear between the menu and the window edge.
const MARGIN: usize = 4;
/// Upper bound on the menu's glyph scale.
const SCALE_MAX: usize = 4;
/// Item row pitch: a blank row between items, or single-spaced.
const PITCH_ROOMY: usize = 2;
const PITCH_COMPACT: usize = 1;
/// Width of the label column, in glyphs, including the selection marker.
const LABEL_COLS: usize = 13;
const HINT: &str = "F12 close  arrows move  enter select";
/// Widest line in glyphs: the hint, or a label with the longest value.
const BLOCK_COLS: usize = 36;
const WHITE: u32 = 0x00ff_ffff;
const YELLOW: u32 = 0x00ff_d700;
const GREY: u32 = 0x00a0_a0a0;
const BLACK: u32 = 0;
/// The REC indicator pulses between these reds over [`PULSE_MS`].
const REC_DIM: u32 = 0x80;
const REC_BRIGHT: u32 = 0xff;
const PULSE_MS: u64 = 1000;
/// CP437 bullet, the REC indicator's dot.
const BULLET: u8 = 0x07;
/// Each colour channel behind the open menu is scaled by this over 256
/// (about 40% darker).
const DIM: u32 = 154;
/// The CP437 right-pointing triangle marks the selected row.
const MARKER: u8 = 0x10;

/// The menu's rows, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Scaler,
    Fullscreen,
    Mute,
    Recording,
    /// The COM1 null-modem link on or off.
    Serial,
    /// The COM2 modem link on or off.
    Modem,
}
pub const ITEMS: [Item; 6] = [
    Item::Scaler,
    Item::Fullscreen,
    Item::Mute,
    Item::Recording,
    Item::Serial,
    Item::Modem,
];
impl Item {
    fn label(self) -> &'static str {
        match self {
            Item::Scaler => "SCALER:",
            Item::Fullscreen => "FULLSCREEN:",
            Item::Mute => "MUTE:",
            Item::Recording => "RECORDING:",
            Item::Serial => "SERIAL:",
            Item::Modem => "MODEM:",
        }
    }
}

/// A LAN link as the menu shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    pub enabled: bool,
    pub status: LineStatus,
    /// The connected peer's advertised name, when discovery knows it.
    pub peer: Option<String>,
    /// The guest's COM port for this link, shown once connected.
    pub com: u8,
}
impl Link {
    /// A call is up or being placed: the peer would notice a pause.
    fn in_use(&self) -> bool {
        matches!(
            self.status,
            LineStatus::Connecting(_) | LineStatus::Connected(_)
        )
    }
}

/// Current frontend state the menu shows and steps from.
#[derive(Clone, Debug)]
pub struct Values {
    pub scaler: Scaler,
    pub fullscreen: bool,
    pub mute: bool,
    /// Mute cannot change (no audio device was opened).
    pub mute_locked: bool,
    /// Emulated milliseconds into the active recording, if any.
    pub recording: Option<u64>,
    pub serial: Link,
    pub modem: Link,
}
impl Values {
    /// The guest is held still while the menu is open, unless a link is in
    /// use: the peer would lose the connection.
    pub fn pauses(&self) -> bool {
        !self.serial.in_use() && !self.modem.in_use()
    }
}

/// Host overlay inputs shared by presenters, independent of their output format.
/// A presenter can composite into its window buffer or use these same inputs to
/// build separate RGBA layers with coverage without changing the frontend.
pub struct Layers<'a> {
    pub menu: &'a Menu,
    pub values: &'a Values,
    /// Host milliseconds driving the REC pulse, sampled after drawing the menu.
    pub pulse_ms: &'a dyn Fn() -> u64,
}
impl Layers<'_> {
    /// Composite at window resolution, after guest scaling: menu first, then REC.
    /// Keep the menu's integer dimming and opaque black glyph outlines intact;
    /// a future RGBA renderer must distinguish those outlines from no coverage.
    pub fn render(&self, buffer: &mut [u32], width: usize, height: usize) {
        self.menu.render(buffer, width, height, self.values);
        render_recording(
            buffer,
            width,
            height,
            self.values.recording,
            (self.pulse_ms)(),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Select,
    Close,
}
impl Key {
    /// Menu meaning of a physical key; F12 is handled as a hotkey instead.
    pub fn from_code(code: KeyCode) -> Option<Key> {
        Some(match code {
            KeyCode::ArrowUp | KeyCode::Numpad8 => Key::Up,
            KeyCode::ArrowDown | KeyCode::Numpad2 => Key::Down,
            KeyCode::ArrowLeft | KeyCode::Numpad4 => Key::Left,
            KeyCode::ArrowRight | KeyCode::Numpad6 => Key::Right,
            KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space => Key::Select,
            KeyCode::Escape => Key::Close,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    Scaler(Scaler),
    Fullscreen(bool),
    Mute(bool),
    /// Start (true) or stop (false) recording.
    Record(bool),
    /// Enable or disable the COM1 serial link.
    Serial(bool),
    /// Enable or disable the COM2 modem link.
    Modem(bool),
}

#[derive(Default)]
pub struct Menu {
    open: bool,
    selected: usize,
}
impl Menu {
    pub fn is_open(&self) -> bool {
        self.open
    }
    /// Opens on the first row, or closes. Returns the new state.
    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        self.selected = 0;
        self.open
    }
    pub fn selected(&self) -> Item {
        ITEMS[self.selected]
    }
    /// Applies one key press. `Close` closes the menu.
    pub fn handle(&mut self, key: Key, values: &Values) -> Action {
        if !self.open {
            return Action::None;
        }
        let action = match key {
            Key::Up => {
                self.selected = (self.selected + ITEMS.len() - 1) % ITEMS.len();
                Action::None
            }
            Key::Down => {
                self.selected = (self.selected + 1) % ITEMS.len();
                Action::None
            }
            Key::Close => Action::Close,
            Key::Left => self.step(false, values),
            Key::Right => self.step(true, values),
            Key::Select => match self.selected() {
                Item::Recording => Action::Record(values.recording.is_none()),
                _ => self.step(true, values),
            },
        };
        if action == Action::Close {
            self.open = false;
            self.selected = 0;
        }
        action
    }
    fn step(&self, forward: bool, values: &Values) -> Action {
        match self.selected() {
            Item::Scaler => Action::Scaler(values.scaler.cycle(forward)),
            Item::Fullscreen => Action::Fullscreen(!values.fullscreen),
            Item::Mute if values.mute_locked => Action::None,
            Item::Mute => Action::Mute(!values.mute),
            Item::Serial => Action::Serial(!values.serial.enabled),
            Item::Modem => Action::Modem(!values.modem.enabled),
            Item::Recording => Action::None,
        }
    }
    /// Dims the frame and draws the menu over it; does nothing when closed.
    pub fn render(&self, buffer: &mut [u32], width: usize, height: usize, values: &Values) {
        if !self.open || buffer.len() < width * height {
            return;
        }
        let buffer = &mut buffer[..width * height];
        dim(buffer);
        let layout = layout(width, height, ITEMS.len());
        let mut canvas = Canvas {
            buffer,
            width,
            height,
            origin_x: 0,
            origin_y: 0,
            scale: layout.scale,
            coverage: false,
        };
        draw_menu(&mut canvas, width, height, &MenuText::new(self, values));
    }
}

fn row_text(item: Item, value: &str, selected: bool) -> String {
    let mut row = vec![if selected { MARKER } else { b' ' }, b' '];
    row.extend_from_slice(item.label().as_bytes());
    if !value.is_empty() {
        row.resize(LABEL_COLS, b' ');
        row.extend_from_slice(value.as_bytes());
    }
    String::from_utf8(row).expect("menu labels and values are ASCII")
}

#[derive(Clone, PartialEq, Eq)]
struct MenuText {
    title: String,
    rows: [String; ITEMS.len()],
    row_colours: [u32; ITEMS.len()],
    // Keep the semantic state explicit in the cache key as well as its text.
    selected: usize,
    paused: bool,
    mute_locked: bool,
}
impl MenuText {
    fn new(menu: &Menu, values: &Values) -> Self {
        let selected = menu.selected;
        let rows = std::array::from_fn(|index| {
            let item = ITEMS[index];
            row_text(item, &value_text(item, values), index == selected)
        });
        let row_colours = std::array::from_fn(|index| {
            let locked = ITEMS[index] == Item::Mute && values.mute_locked;
            match (index == selected, locked) {
                (_, true) => GREY,
                (true, false) => YELLOW,
                (false, false) => WHITE,
            }
        });
        let paused = values.pauses();
        Self {
            title: if paused {
                "gp64  PAUSED"
            } else {
                "gp64  RUNNING (LINK IN USE)"
            }
            .into(),
            rows,
            row_colours,
            selected,
            paused,
            mute_locked: values.mute_locked,
        }
    }
}

fn draw_menu(canvas: &mut Canvas<'_>, width: usize, height: usize, text: &MenuText) {
    let layout = layout(width, height, ITEMS.len());
    canvas.scale = layout.scale;
    let line = GLYPH_H * layout.scale;
    let mut y = layout.y;
    canvas.text(layout.x, y, text.title.as_bytes(), WHITE);
    y += 2 * line;
    for index in 0..ITEMS.len() {
        canvas.text(
            layout.x,
            y,
            text.rows[index].as_bytes(),
            text.row_colours[index],
        );
        y += layout.pitch * line;
    }
    // One blank row after the last item, then the hint.
    let hint = y + 2 * line - layout.pitch * line;
    canvas.text(layout.x, hint, HINT.as_bytes(), GREY);
}

fn ascii_uppercase(value: &str, limit: usize) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii() {
                character.to_ascii_uppercase()
            } else {
                '?'
            }
        })
        .take(limit)
        .collect()
}

fn value_text(item: Item, values: &Values) -> String {
    let on = |on: bool| if on { "ON" } else { "OFF" };
    match item {
        Item::Scaler => values.scaler.label().into(),
        Item::Fullscreen => on(values.fullscreen).into(),
        Item::Mute if values.mute_locked => "ON (--mute)".into(),
        Item::Mute => on(values.mute).into(),
        Item::Recording => values
            .recording
            .map_or_else(|| "OFF".into(), |ms| format!("ON {}", clock(ms))),
        Item::Serial => link_text(&values.serial),
        Item::Modem => link_text(&values.modem),
    }
}

/// Glyphs available for a row's value.
const VALUE_COLS: usize = BLOCK_COLS - LABEL_COLS;
/// OFF, or ON and what the link is doing, in one row. A connected link
/// names its peer and the COM port to pick in the program's setup; a long
/// name is cut so the port always shows.
fn link_text(link: &Link) -> String {
    if !link.enabled {
        return "OFF".into();
    }
    let state = match &link.status {
        LineStatus::Off | LineStatus::Listening(_) => "SEARCHING".into(),
        LineStatus::Connecting(_) => "CONNECTING".into(),
        LineStatus::Connected(peer) => {
            // The peer's port is ephemeral; its name or address is what matters.
            let name = link.peer.clone().unwrap_or_else(|| {
                peer.rsplit_once(':')
                    .map_or(peer.as_str(), |(host, _)| host)
                    .to_owned()
            });
            let port = format!(" (COM{})", link.com);
            let room = VALUE_COLS - "ON - ".len() - port.len();
            return format!("ON - {}{port}", ascii_uppercase(&name, room));
        }
        LineStatus::Failed(reason) => format!("FAILED: {reason}"),
    };
    ascii_uppercase(&format!("ON - {state}"), VALUE_COLS)
}

/// Draws a pulsing red dot and `REC mm:ss` in the top-right corner while
/// `recording` holds the emulated milliseconds since it started; draws
/// nothing otherwise. Like the menu it goes into the window buffer after
/// scaling, so it never reaches the guest frame or the recording. `pulse_ms`
/// is any steadily advancing host clock.
pub fn render_recording(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    recording: Option<u64>,
    pulse_ms: u64,
) {
    let Some(ms) = recording else {
        return;
    };
    if buffer.len() < width * height {
        return;
    }
    let (text, red) = recording_appearance(ms, pulse_ms);
    let layout = recording_layout(width, height, text.len());
    let mut canvas = Canvas {
        buffer: &mut buffer[..width * height],
        width,
        height,
        origin_x: 0,
        origin_y: 0,
        scale: layout.scale,
        coverage: false,
    };
    draw_recording(&mut canvas, width, height, &text, red);
}

fn recording_appearance(ms: u64, pulse_ms: u64) -> (Vec<u8>, u32) {
    let mut text = vec![BULLET];
    text.extend_from_slice(format!(" REC {}", clock(ms)).as_bytes());
    // One scale step per 360 window rows, up to the menu's largest.
    // Raised cosine: bright at the start of each cycle, dim half-way.
    let phase = (pulse_ms % PULSE_MS) as f64 / PULSE_MS as f64;
    let t = (1.0 + (phase * std::f64::consts::TAU).cos()) / 2.0;
    let red = REC_DIM + (t * f64::from(REC_BRIGHT - REC_DIM)).round() as u32;
    (text, red)
}

#[derive(Clone, Copy)]
struct RecordingLayout {
    scale: usize,
    x: usize,
    y: usize,
    text_width: usize,
    text_height: usize,
}
fn recording_layout(width: usize, height: usize, text_len: usize) -> RecordingLayout {
    let scale = (height / 360).clamp(1, SCALE_MAX);
    let text_width = text_len * GLYPH_W * scale;
    RecordingLayout {
        scale,
        x: width.saturating_sub(text_width + MARGIN * scale),
        y: MARGIN * scale,
        text_width,
        text_height: GLYPH_H * scale,
    }
}
fn draw_recording(canvas: &mut Canvas<'_>, width: usize, height: usize, text: &[u8], red: u32) {
    let layout = recording_layout(width, height, text.len());
    canvas.scale = layout.scale;
    canvas.text(layout.x, layout.y, text, red << 16);
}

/// Scales each colour channel to [`DIM`]/256.
pub fn dim(buffer: &mut [u32]) {
    for pixel in buffer {
        let red_blue = (((*pixel & 0x00ff_00ff) * DIM) >> 8) & 0x00ff_00ff;
        let green = (((*pixel & 0x0000_ff00) * DIM) >> 8) & 0x0000_ff00;
        *pixel = red_blue | green;
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Layout {
    scale: usize,
    pitch: usize,
    x: usize,
    y: usize,
}
/// Glyph rows the block needs: title, gap, items at `pitch`, gap, hint.
fn block_rows(pitch: usize, item_count: usize) -> usize {
    5 + item_count.saturating_sub(1) * pitch
}
/// Largest scale in `1..=SCALE_MAX` at which the block fits inside the
/// margins; 1 when nothing fits (the text is then clipped, never written
/// out of bounds).
fn fit_scale(width: usize, height: usize, rows: usize) -> usize {
    let by_width = width.saturating_sub(2 * MARGIN) / (BLOCK_COLS * GLYPH_W);
    let by_height = height.saturating_sub(2 * MARGIN) / (rows * GLYPH_H);
    by_width.min(by_height).clamp(1, SCALE_MAX)
}
/// Prefers roomy spacing, but single-spaces the items when that buys a
/// larger, more readable scale or roomy spacing does not fit at all.
fn layout(width: usize, height: usize, item_count: usize) -> Layout {
    let fits =
        |pitch, scale| block_rows(pitch, item_count) * GLYPH_H * scale + 2 * MARGIN <= height;
    let roomy = fit_scale(width, height, block_rows(PITCH_ROOMY, item_count));
    let compact = fit_scale(width, height, block_rows(PITCH_COMPACT, item_count));
    let (scale, pitch) = if fits(PITCH_ROOMY, roomy) && (roomy >= 2 || compact <= roomy) {
        (roomy, PITCH_ROOMY)
    } else {
        (compact, PITCH_COMPACT)
    };
    let block_w = BLOCK_COLS * GLYPH_W * scale;
    let block_h = block_rows(pitch, item_count) * GLYPH_H * scale;
    Layout {
        scale,
        pitch,
        x: width.saturating_sub(block_w) / 2,
        y: height.saturating_sub(block_h) / 2,
    }
}

struct Canvas<'a> {
    buffer: &'a mut [u32],
    width: usize,
    height: usize,
    origin_x: usize,
    origin_y: usize,
    scale: usize,
    coverage: bool,
}
impl Canvas<'_> {
    /// CP437 glyphs with a one-glyph-pixel black outline.
    fn text(&mut self, x: usize, y: usize, text: &[u8], colour: u32) {
        let (x, y, s) = (x as isize, y as isize, self.scale as isize);
        for (dx, dy, colour) in [
            (-s, 0, BLACK),
            (s, 0, BLACK),
            (0, -s, BLACK),
            (0, s, BLACK),
            (0, 0, colour),
        ] {
            for (index, &glyph) in text.iter().enumerate() {
                let left = x + dx + (index * GLYPH_W) as isize * s;
                self.glyph(left, y + dy, glyph, colour);
            }
        }
    }
    fn glyph(&mut self, x: isize, y: isize, glyph: u8, colour: u32) {
        let scale = self.scale as isize;
        for row in 0..GLYPH_H {
            let bits = CP437_FONT_8X16[usize::from(glyph) * GLYPH_H + row];
            for column in 0..GLYPH_W {
                if bits & (0x80 >> column) == 0 {
                    continue;
                }
                for sy in 0..scale {
                    let py = y + row as isize * scale + sy;
                    let local_y = py - self.origin_y as isize;
                    if local_y < 0 || local_y as usize >= self.height {
                        continue;
                    }
                    for sx in 0..scale {
                        let px = x + column as isize * scale + sx;
                        let local_x = px - self.origin_x as isize;
                        if local_x >= 0 && (local_x as usize) < self.width {
                            let pixel = if self.coverage {
                                0xff00_0000 | colour
                            } else {
                                colour
                            };
                            self.buffer[local_y as usize * self.width + local_x as usize] = pixel;
                        }
                    }
                }
            }
        }
    }
}

/// Cropped host overlay pixels for the GPU presenter. Pixels are packed as
/// 0xAARRGGBB (BGRA in memory on these little-endian targets); the high byte
/// is coverage (zero or 255), and covered black pixels remain opaque.
#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Raster {
    pub(crate) origin: [u32; 2],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u32>,
}

/// Caches the menu glyph layer independently from the caller's visibility
/// flag and the frame dimming pass.
#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Default)]
pub(crate) struct MenuRasterCache {
    key: Option<MenuRasterKey>,
    raster: Raster,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Clone, PartialEq, Eq)]
struct MenuRasterKey {
    width: u32,
    height: u32,
    text: MenuText,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl MenuRasterCache {
    /// Rebuilds the glyph texture only when its rendered content or window
    /// dimensions change. The caller controls visibility separately.
    pub(crate) fn update(&mut self, menu: &Menu, width: u32, height: u32, values: &Values) -> bool {
        if !menu.open {
            return false;
        }
        let text = MenuText::new(menu, values);
        let key = MenuRasterKey {
            width,
            height,
            text: text.clone(),
        };
        if self.key.as_ref() == Some(&key) {
            return false;
        }
        self.raster = menu_raster(width, height, &text);
        self.key = Some(key);
        true
    }

    pub(crate) fn raster(&self) -> &Raster {
        &self.raster
    }
}

/// Caches the small top-right REC layer. Passing no recording leaves the
/// previous texture available; the presenter owns visibility.
#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Default)]
pub(crate) struct RecRasterCache {
    key: Option<RecRasterKey>,
    raster: Raster,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Clone, PartialEq, Eq)]
struct RecRasterKey {
    width: u32,
    height: u32,
    clock: String,
    red: u32,
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
impl RecRasterCache {
    /// Rebuilds only when the displayed clock, pulse colour, or window size
    /// changes. A stopped recording keeps its last raster for reuse.
    pub(crate) fn update(
        &mut self,
        recording: Option<u64>,
        width: u32,
        height: u32,
        pulse_ms: u64,
    ) -> bool {
        let Some(ms) = recording else {
            return false;
        };
        let (text, red) = recording_appearance(ms, pulse_ms);
        let clock = clock(ms);
        let key = RecRasterKey {
            width,
            height,
            clock,
            red,
        };
        if self.key.as_ref() == Some(&key) {
            return false;
        }
        self.raster = recording_raster(width, height, &text, red);
        self.key = Some(key);
        true
    }

    pub(crate) fn raster(&self) -> Option<&Raster> {
        self.key.as_ref().map(|_| &self.raster)
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn menu_raster(width: u32, height: u32, text: &MenuText) -> Raster {
    let width = width as usize;
    let height = height as usize;
    let layout = layout(width, height, ITEMS.len());
    let block_width = BLOCK_COLS * GLYPH_W * layout.scale;
    let block_height = block_rows(layout.pitch, ITEMS.len()) * GLYPH_H * layout.scale;
    let bounds = cropped_bounds(
        width,
        height,
        layout.x,
        layout.y,
        block_width,
        block_height,
        layout.scale,
    );
    let mut raster = raster_for_bounds(bounds);
    if raster.width == 0 || raster.height == 0 {
        return raster;
    }
    let mut canvas = Canvas {
        buffer: &mut raster.pixels,
        width: raster.width as usize,
        height: raster.height as usize,
        origin_x: raster.origin[0] as usize,
        origin_y: raster.origin[1] as usize,
        scale: layout.scale,
        coverage: true,
    };
    draw_menu(&mut canvas, width, height, text);
    raster
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn recording_raster(width: u32, height: u32, text: &[u8], red: u32) -> Raster {
    let width = width as usize;
    let height = height as usize;
    let layout = recording_layout(width, height, text.len());
    let bounds = cropped_bounds(
        width,
        height,
        layout.x,
        layout.y,
        layout.text_width,
        layout.text_height,
        layout.scale,
    );
    let mut raster = raster_for_bounds(bounds);
    if raster.width == 0 || raster.height == 0 {
        return raster;
    }
    let mut canvas = Canvas {
        buffer: &mut raster.pixels,
        width: raster.width as usize,
        height: raster.height as usize,
        origin_x: raster.origin[0] as usize,
        origin_y: raster.origin[1] as usize,
        scale: layout.scale,
        coverage: true,
    };
    draw_recording(&mut canvas, width, height, text, red);
    raster
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn cropped_bounds(
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    content_width: usize,
    content_height: usize,
    scale: usize,
) -> (usize, usize, usize, usize) {
    let left = x.saturating_sub(scale);
    let top = y.saturating_sub(scale);
    let right = x
        .saturating_add(content_width)
        .saturating_add(scale)
        .min(width);
    let bottom = y
        .saturating_add(content_height)
        .saturating_add(scale)
        .min(height);
    (
        left.min(width),
        top.min(height),
        right.max(left.min(width)),
        bottom.max(top.min(height)),
    )
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn raster_for_bounds((left, top, right, bottom): (usize, usize, usize, usize)) -> Raster {
    let width = right.saturating_sub(left);
    let height = bottom.saturating_sub(top);
    Raster {
        origin: [left as u32, top as u32],
        width: width as u32,
        height: height as u32,
        pixels: vec![0; width * height],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values {
            scaler: Scaler::Integer,
            fullscreen: false,
            mute: false,
            mute_locked: false,
            recording: None,
            serial: Link::default(),
            modem: Link::default(),
        }
    }
    fn open() -> Menu {
        let mut menu = Menu::default();
        assert!(menu.toggle());
        menu
    }
    fn select(menu: &mut Menu, item: Item) {
        while menu.selected() != item {
            menu.handle(Key::Down, &values());
        }
    }
    fn link(enabled: bool, status: LineStatus) -> Link {
        Link {
            enabled,
            status,
            peer: None,
            com: 1,
        }
    }
    /// Window sizes from tiny to large, in both orientations.
    const WINDOWS: &[(usize, usize)] = &[
        (1, 1),
        (40, 30),
        (160, 120),
        (300, 250),
        (320, 200),
        (640, 400),
        (800, 600),
        (960, 720),
        (1280, 720),
        (1920, 1080),
        (2560, 1440),
        (3840, 2160),
        (600, 1600),
    ];

    #[test]
    fn toggle_opens_on_the_first_row_and_closes() {
        let mut menu = Menu::default();
        assert!(!menu.is_open());
        assert!(menu.toggle());
        menu.handle(Key::Down, &values());
        assert!(!menu.toggle());
        assert!(menu.toggle());
        assert_eq!(menu.selected(), Item::Scaler);
        // A closed menu ignores keys.
        menu.toggle();
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
    }

    #[test]
    fn up_and_down_wrap() {
        let mut menu = open();
        assert_eq!(menu.handle(Key::Up, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Modem);
        assert_eq!(menu.handle(Key::Down, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Scaler);
        for item in ITEMS.into_iter().skip(1) {
            menu.handle(Key::Down, &values());
            assert_eq!(menu.selected(), item);
        }
    }

    #[test]
    fn value_rows_cycle_both_ways_and_emit_actions() {
        let mut menu = open();
        let v = values();
        assert_eq!(menu.handle(Key::Right, &v), Action::Scaler(Scaler::Smooth));
        assert_eq!(menu.handle(Key::Left, &v), Action::Scaler(Scaler::Crt));
        assert_eq!(menu.handle(Key::Select, &v), Action::Scaler(Scaler::Smooth));
        select(&mut menu, Item::Fullscreen);
        assert_eq!(menu.handle(Key::Select, &v), Action::Fullscreen(true));
        select(&mut menu, Item::Mute);
        assert_eq!(menu.handle(Key::Left, &v), Action::Mute(true));
        // Value rows never close the menu.
        assert!(menu.is_open());
    }

    #[test]
    fn link_rows_switch_their_link_on_and_off() {
        let mut menu = open();
        let off = values();
        let on = Values {
            serial: link(true, LineStatus::Listening(1)),
            modem: link(true, LineStatus::Listening(2)),
            ..values()
        };
        select(&mut menu, Item::Serial);
        assert_eq!(menu.handle(Key::Select, &off), Action::Serial(true));
        assert_eq!(menu.handle(Key::Left, &on), Action::Serial(false));
        select(&mut menu, Item::Modem);
        assert_eq!(menu.handle(Key::Right, &off), Action::Modem(true));
        assert_eq!(menu.handle(Key::Select, &on), Action::Modem(false));
        assert!(menu.is_open());
    }

    #[test]
    fn locked_rows_do_nothing() {
        let mut menu = open();
        let v = Values {
            mute_locked: true,
            recording: Some(0),
            ..values()
        };
        select(&mut menu, Item::Mute);
        assert_eq!(menu.handle(Key::Right, &v), Action::None);
    }

    #[test]
    fn escape_closes_the_menu() {
        let mut menu = open();
        select(&mut menu, Item::Modem);
        assert_eq!(menu.handle(Key::Close, &values()), Action::Close);
        assert!(!menu.is_open());
        assert!(menu.toggle());
        assert_eq!(menu.selected(), Item::Scaler);
    }

    #[test]
    fn keys_map_to_menu_keys() {
        assert_eq!(Key::from_code(KeyCode::ArrowUp), Some(Key::Up));
        assert_eq!(Key::from_code(KeyCode::Escape), Some(Key::Close));
        assert_eq!(Key::from_code(KeyCode::Enter), Some(Key::Select));
        assert_eq!(Key::from_code(KeyCode::KeyA), None);
        assert_eq!(Key::from_code(KeyCode::F12), None);
    }

    #[test]
    fn every_line_fits_the_block_width() {
        let connected = |peer: &str| Link {
            enabled: true,
            status: LineStatus::Connected("[2001:db8::1234:5678:9abc:def0]:23456".into()),
            peer: (!peer.is_empty()).then(|| peer.to_owned()),
            com: 2,
        };
        let v = Values {
            mute_locked: true,
            recording: Some(999 * 60_000 + 59_999),
            serial: connected("a-very-long-workstation-name-indeed"),
            modem: connected(""),
            ..values()
        };
        assert!(HINT.len() <= BLOCK_COLS);
        assert!("gp64  RUNNING (LINK IN USE)".len() <= BLOCK_COLS);
        for item in ITEMS {
            assert!(item.label().len() + 2 <= LABEL_COLS, "{item:?}");
            assert!(
                LABEL_COLS + value_text(item, &v).len() <= BLOCK_COLS,
                "{item:?}"
            );
        }
        let failed = link(true, LineStatus::Failed("x".repeat(40)));
        assert!(link_text(&failed).len() <= VALUE_COLS);
    }

    #[test]
    fn link_rows_show_off_or_what_the_link_is_doing() {
        assert_eq!(link_text(&link(false, LineStatus::Off)), "OFF");
        // A hang-up still in progress already reads as off.
        assert_eq!(
            link_text(&link(false, LineStatus::Connected("10.0.0.2:4000".into()))),
            "OFF"
        );
        assert_eq!(link_text(&link(true, LineStatus::Off)), "ON - SEARCHING");
        assert_eq!(
            link_text(&link(true, LineStatus::Listening(4000))),
            "ON - SEARCHING"
        );
        assert_eq!(
            link_text(&link(true, LineStatus::Connecting("10.0.0.2:4000".into()))),
            "ON - CONNECTING"
        );
        assert_eq!(
            link_text(&link(true, LineStatus::Connected("10.0.0.2:4000".into()))),
            "ON - 10.0.0.2 (COM1)"
        );
        let named = Link {
            peer: Some("desk".into()),
            com: 2,
            ..link(true, LineStatus::Connected("10.0.0.2:4000".into()))
        };
        assert_eq!(link_text(&named), "ON - DESK (COM2)");
        // A long name gives way so the port always shows.
        let long = Link {
            peer: Some("a-very-long-workstation-name".into()),
            ..named
        };
        assert_eq!(link_text(&long), "ON - A-VERY-LONG (COM2)");
        assert_eq!(
            link_text(&link(true, LineStatus::Failed("refused".into()))),
            "ON - FAILED: REFUSED"
        );
    }

    #[test]
    fn the_menu_pauses_only_while_no_link_is_in_use() {
        for (status, pauses) in [
            (LineStatus::Off, true),
            (LineStatus::Failed("refused".into()), true),
            (LineStatus::Listening(1), true),
            (LineStatus::Connecting("h:1".into()), false),
            (LineStatus::Connected("10.0.0.2:4000".into()), false),
        ] {
            let serial = Values {
                serial: link(true, status.clone()),
                ..values()
            };
            let modem = Values {
                modem: link(true, status.clone()),
                ..values()
            };
            assert_eq!(serial.pauses(), pauses, "serial {status:?}");
            assert_eq!(modem.pauses(), pauses, "modem {status:?}");
        }
    }

    #[test]
    fn title_shows_whether_the_guest_is_paused() {
        let (width, height) = (960, 720);
        let mut paused = vec![0; width * height];
        let mut running = vec![0; width * height];
        open().render(&mut paused, width, height, &values());
        let v = Values {
            modem: link(true, LineStatus::Connected("10.0.0.2:4000".into())),
            ..values()
        };
        open().render(&mut running, width, height, &v);
        assert_ne!(paused, running);
    }

    #[test]
    fn menu_never_overflows_the_window() {
        for rows in [1, 2, ITEMS.len()] {
            for &(width, height) in WINDOWS {
                let layout = layout(width, height, rows);
                assert!((1..=SCALE_MAX).contains(&layout.scale));
                let need_w = BLOCK_COLS * GLYPH_W * layout.scale;
                let need_h = block_rows(layout.pitch, rows) * GLYPH_H * layout.scale;
                let fittable = block_rows(PITCH_COMPACT, rows) * GLYPH_H + 2 * MARGIN <= height
                    && BLOCK_COLS * GLYPH_W + 2 * MARGIN <= width;
                if fittable {
                    assert!(
                        layout.x + need_w <= width && layout.y + need_h <= height,
                        "{width}x{height}: {layout:?} needs {need_w}x{need_h}"
                    );
                    assert!(layout.x >= MARGIN && layout.y >= MARGIN, "{width}x{height}");
                } else {
                    assert_eq!(layout.scale, 1, "{width}x{height}");
                }
            }
        }
    }

    #[test]
    fn layout_scales_with_the_window() {
        assert_eq!(layout(320, 200, ITEMS.len()).scale, 1);
        // At 640x400 the menu gains a larger compact scale.
        assert_eq!(
            layout(640, 400, ITEMS.len()),
            layout_of(2, PITCH_COMPACT, 640, 400, ITEMS.len())
        );
        assert_eq!(
            layout(1920, 1080, ITEMS.len()),
            layout_of(4, PITCH_ROOMY, 1920, 1080, ITEMS.len())
        );
        assert_eq!(layout(3840, 2160, ITEMS.len()).scale, SCALE_MAX);
        // Narrow and tall: width decides.
        assert_eq!(layout(600, 1600, ITEMS.len()).scale, 2);
    }
    fn layout_of(
        scale: usize,
        pitch: usize,
        width: usize,
        height: usize,
        item_count: usize,
    ) -> Layout {
        Layout {
            scale,
            pitch,
            x: (width - BLOCK_COLS * GLYPH_W * scale) / 2,
            y: (height - block_rows(pitch, item_count) * GLYPH_H * scale) / 2,
        }
    }

    #[test]
    fn render_draws_inside_the_buffer_at_every_size() {
        let menu = open();
        for &(width, height) in WINDOWS {
            // A guard word after the buffer catches any write past the end.
            let mut buffer = vec![0x0080_8080; width * height + 1];
            menu.render(&mut buffer[..width * height], width, height, &values());
            assert_eq!(buffer[width * height], 0x0080_8080, "{width}x{height}");
            if width >= 320 && height >= 200 {
                assert!(buffer.contains(&YELLOW), "{width}x{height}");
                assert!(buffer.contains(&WHITE), "{width}x{height}");
                assert!(buffer.contains(&BLACK), "{width}x{height}");
            }
        }
        // A buffer shorter than the window is left alone.
        let mut short = vec![7; 10];
        open().render(&mut short, 640, 480, &values());
        assert_eq!(short, [7; 10]);
    }

    #[test]
    fn frame_is_dimmed_only_while_open() {
        let (width, height) = (960, 720);
        let mut buffer = vec![0x00ff_8040; width * height];
        let mut menu = Menu::default();
        menu.render(&mut buffer, width, height, &values());
        assert!(buffer.iter().all(|&pixel| pixel == 0x00ff_8040));
        menu.toggle();
        menu.render(&mut buffer, width, height, &values());
        // A corner is outside the text block: only dimmed.
        assert_eq!(buffer[0], 0x0099_4d26);
        assert_eq!(buffer[width * height - 1], 0x0099_4d26);
    }

    #[test]
    fn dim_scales_channels_without_bleeding() {
        let mut pixels = [0x00ff_ffff, 0, 0x0001_0101, 0xff00_00ff];
        dim(&mut pixels);
        assert_eq!(pixels, [0x0099_9999, 0, 0, 0x0000_0099]);
    }

    #[test]
    fn glyphs_clip_at_every_edge() {
        let (width, height) = (20, 20);
        let mut buffer = vec![0; width * height];
        let mut canvas = Canvas {
            buffer: &mut buffer,
            width,
            height,
            origin_x: 0,
            origin_y: 0,
            scale: 3,
            coverage: false,
        };
        for (x, y) in [(-30, -30), (-5, 5), (15, 15), (19, -2), (100, 100)] {
            canvas.glyph(x, y, b'#', WHITE);
        }
        canvas.text(0, 0, b"WIDE TEXT", YELLOW);
        assert!(buffer.contains(&WHITE) && buffer.contains(&YELLOW));
    }

    #[test]
    fn recording_row_toggles_on_enter_only() {
        let mut menu = open();
        select(&mut menu, Item::Recording);
        let off = values();
        assert_eq!(value_text(Item::Recording, &off), "OFF");
        assert_eq!(menu.handle(Key::Select, &off), Action::Record(true));
        let on = Values {
            recording: Some(61_500),
            ..values()
        };
        assert_eq!(value_text(Item::Recording, &on), "ON 01:01");
        assert_eq!(menu.handle(Key::Select, &on), Action::Record(false));
        assert_eq!(menu.handle(Key::Left, &on), Action::None);
        assert_eq!(menu.handle(Key::Right, &off), Action::None);
        assert!(menu.is_open());
    }

    fn is_red(pixel: u32) -> bool {
        pixel & 0xffff == 0 && (pixel >> 16) >= REC_DIM
    }

    #[test]
    fn rec_indicator_is_drawn_only_while_recording() {
        let (width, height) = (960, 720);
        let mut buffer = vec![0x0012_3456; width * height];
        render_recording(&mut buffer, width, height, None, 0);
        assert!(buffer.iter().all(|&pixel| pixel == 0x0012_3456));
        render_recording(&mut buffer, width, height, Some(5_000), 0);
        assert!(buffer.iter().any(|&pixel| is_red(pixel)));
        // Top-right corner only: the left half and the lower rows stay as they were.
        for y in 0..height {
            for x in 0..width {
                if x < width / 2 || y > height / 4 {
                    assert_eq!(buffer[y * width + x], 0x0012_3456, "{x},{y}");
                }
            }
        }
    }

    #[test]
    fn rec_indicator_stays_inside_the_buffer_at_every_size() {
        for &(width, height) in WINDOWS {
            let mut buffer = vec![0x0080_8080; width * height + 1];
            render_recording(
                &mut buffer[..width * height],
                width,
                height,
                Some(3_723_000),
                0,
            );
            assert_eq!(buffer[width * height], 0x0080_8080, "{width}x{height}");
            if width >= 160 && height >= 120 {
                assert!(
                    buffer.iter().any(|&pixel| is_red(pixel)),
                    "{width}x{height}"
                );
            }
        }
        let mut short = vec![7; 10];
        render_recording(&mut short, 640, 480, Some(0), 0);
        assert_eq!(short, [7; 10]);
    }

    #[test]
    fn rec_indicator_pulses_on_a_one_second_cycle() {
        let (width, height) = (640, 480);
        let draw = |pulse| {
            let mut buffer = vec![0; width * height];
            render_recording(&mut buffer, width, height, Some(0), pulse);
            buffer
                .into_iter()
                .filter(|&pixel| is_red(pixel))
                .max()
                .unwrap()
        };
        assert_eq!(draw(0), REC_BRIGHT << 16);
        assert_eq!(draw(500), REC_DIM << 16);
        assert_eq!(draw(1000), draw(0));
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    fn composite_raster(buffer: &mut [u32], window_width: usize, raster: &Raster) {
        for y in 0..raster.height as usize {
            for x in 0..raster.width as usize {
                let source = raster.pixels[y * raster.width as usize + x];
                if source >> 24 != 0 {
                    let target_x = raster.origin[0] as usize + x;
                    let target_y = raster.origin[1] as usize + y;
                    buffer[target_y * window_width + target_x] = source & 0x00ff_ffff;
                }
            }
        }
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn cropped_rasters_match_menu_dimming_and_recording_composition() {
        let menu = open();
        let values = Values {
            recording: Some(61_500),
            ..values()
        };
        let windows = [
            (1, 1),
            (17, 19),
            (319, 201),
            (641, 401),
            (1280, 721),
            (3840, 2160),
        ];
        let mut menu_cache = MenuRasterCache::default();
        let mut rec_cache = RecRasterCache::default();
        for (width, height) in windows {
            let base = vec![0x00ff_8040; width * height];
            let mut expected = base.clone();
            menu.render(&mut expected, width, height, &values);
            render_recording(&mut expected, width, height, values.recording, 0);

            let mut actual = base;
            if menu.is_open() {
                dim(&mut actual);
            }
            assert!(menu_cache.update(&menu, width as u32, height as u32, &values));
            let menu_raster = menu_cache.raster();
            assert!(menu_raster
                .pixels
                .iter()
                .all(|pixel| matches!(pixel >> 24, 0 | 255)));
            composite_raster(&mut actual, width, menu_raster);

            assert!(rec_cache.update(values.recording, width as u32, height as u32, 0));
            let rec_raster = rec_cache.raster().unwrap();
            assert!(rec_raster
                .pixels
                .iter()
                .all(|pixel| matches!(pixel >> 24, 0 | 255)));
            composite_raster(&mut actual, width, rec_raster);
            assert!(
                actual == expected,
                "{width}x{height}: raster composition differs"
            );
        }

        // A black outline is covered black, not an uncovered transparent pixel.
        assert!(menu_cache.raster().pixels.contains(&0xff00_0000));
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn menu_raster_cache_tracks_visible_content_and_reuses_closed_menu() {
        let mut menu = open();
        let mut values = values();
        let mut cache = MenuRasterCache::default();
        assert!(cache.update(&menu, 641, 401, &values));
        assert!(!cache.update(&menu, 641, 401, &values));

        values.recording = Some(61_000);
        assert!(cache.update(&menu, 641, 401, &values));
        values.recording = Some(61_999);
        assert!(!cache.update(&menu, 641, 401, &values));

        menu.handle(Key::Down, &values);
        assert!(cache.update(&menu, 641, 401, &values));
        let running = Values {
            serial: link(true, LineStatus::Connecting("host:1".into())),
            ..values.clone()
        };
        assert!(cache.update(&menu, 641, 401, &running));
        let locked = Values {
            mute_locked: true,
            ..running
        };
        assert!(cache.update(&menu, 641, 401, &locked));
        assert!(cache.update(&menu, 643, 401, &locked));
        assert!(!cache.update(&menu, 643, 401, &locked));

        let mut identical_menu = open();
        let mut identical_cache = MenuRasterCache::default();
        assert!(identical_cache.update(&identical_menu, 641, 401, &values));
        assert!(!identical_menu.toggle());
        assert!(!identical_cache.update(&identical_menu, 641, 401, &values));
        assert!(identical_menu.toggle());
        assert!(!identical_cache.update(&identical_menu, 641, 401, &values));
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn recording_raster_cache_tracks_clock_pulse_and_size() {
        let mut cache = RecRasterCache::default();
        assert!(cache.raster().is_none());
        assert!(cache.update(Some(61_000), 641, 401, 0));
        let bright = cache.raster().unwrap().clone();
        assert!(bright.pixels.contains(&0xffff_0000));
        assert!(!cache.update(Some(61_999), 641, 401, 0));
        assert!(cache.update(Some(61_999), 641, 401, 500));
        let dim = cache.raster().unwrap().clone();
        assert!(dim.pixels.contains(&0xff80_0000));
        assert!(!cache.update(None, 641, 401, 500));
        assert_eq!(cache.raster(), Some(&dim));
        assert!(!cache.update(None, 643, 401, 500));
        assert!(cache.update(Some(61_999), 643, 401, 500));
    }
}
