//! The F12 host menu. It is drawn on the CPU into the window buffer after
//! scaling, at window resolution, so it never reaches the guest frame or a
//! recording. The menu model knows nothing about windows or threads: keys
//! go in, [`Action`]s come out, and the frontend applies them. The REC
//! indicator is drawn the same way.
use crate::discovery::Peer;
use crate::scaler::Scaler;
use gp64::recording::clock;
use gp64_hw::vga::CP437_FONT_8X16;
use gp64_machine::modem::LineStatus;
use std::net::IpAddr;
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
const MODEM_HINT: &str = "esc/left back  arrows move  enter";
const PEERS_HINT: &str = "up/down choose  enter join  esc back";
const PEERS_EMPTY_HINT: &str = "esc/left back  searching";
/// The hint while a value is being typed.
const EDIT_HINT: &str = "type  enter start  esc cancel";
/// Longest typed value: a full DNS name and a port.
const EDIT_MAX: usize = 261;
/// Widest line in glyphs: the hint, or a label with the longest value.
const BLOCK_COLS: usize = 36;
/// Maximum discovered peers accepted by the menu, matching discovery's cap.
const PEER_LIMIT: usize = 64;
/// Maximum peer rows shown at once.
const PEER_PAGE_SIZE: usize = 5;
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

/// Rows in the main menu, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Scaler,
    Fullscreen,
    Mute,
    Recording,
    /// Opens the modem setup submenu.
    Modem,
    /// Opens the discovered LAN host list.
    LanHosts,
    /// The phone line's status; nothing to change.
    Line,
    /// The modem's COM port: this run's, and the saved one for the next.
    Com,
    /// Listen for a caller on a TCP port.
    Host,
    /// Dial a `host:port`.
    Join,
    /// A dynamically listed discovered host.
    Peer,
    /// Read-only peer search/error/missing status.
    PeerStatus,
}
pub const ITEMS: [Item; 5] = [
    Item::Scaler,
    Item::Fullscreen,
    Item::Mute,
    Item::Recording,
    Item::Modem,
];
const MODEM_ITEMS: [Item; 5] = [
    Item::Line,
    Item::Com,
    Item::Host,
    Item::Join,
    Item::LanHosts,
];
impl Item {
    fn label(self) -> &'static str {
        match self {
            Item::Scaler => "SCALER:",
            Item::Fullscreen => "FULLSCREEN:",
            Item::Mute => "MUTE:",
            Item::Recording => "RECORDING:",
            Item::Modem => "MODEM:",
            Item::LanHosts => "LAN HOSTS:",
            Item::Line => "LINE:",
            Item::Com => "COM PORT:",
            Item::Host => "HOST PORT:",
            Item::Join => "JOIN:",
            Item::Peer => "HOST:",
            Item::PeerStatus => "STATUS:",
        }
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
    /// The modem's phone line.
    pub modem: LineStatus,
    /// The modem's COM port for this run (1 or 2).
    pub com: u8,
    /// The saved COM port the next launch uses.
    pub next_com: u8,
    /// The port HOST PORT listens on.
    pub host_port: u16,
    /// The `host:port` JOIN dials; empty until one is typed.
    pub join: String,
    /// Hosts found by LAN discovery.
    pub peers: Vec<Peer>,
    /// Latest non-fatal LAN discovery error, if any.
    pub discovery_error: Option<String>,
}
impl Values {
    /// The guest is held still while the menu is open, unless a phone line
    /// is up or coming up: the peer would lose carrier.
    pub fn pauses(&self) -> bool {
        !self.modem.is_active()
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
    /// A typed character, for the HOST PORT and JOIN values.
    Char(char),
    Backspace,
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
            KeyCode::Backspace => Key::Backspace,
            _ => return None,
        })
    }
}

/// Menu keys for one key press. `text` is what the press types. While a
/// value is being typed, typed characters win (numpad digits and Space
/// type rather than move or select); otherwise menu keys win.
pub fn keys(code: KeyCode, text: Option<&str>, editing: bool) -> Vec<Key> {
    let typed: Vec<Key> = text
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .map(Key::Char)
        .collect();
    if editing && !typed.is_empty() {
        return typed;
    }
    match Key::from_code(code) {
        Some(key) => vec![key],
        None => typed,
    }
}

/// A value being typed into a row.
#[derive(Debug, PartialEq, Eq)]
struct Edit {
    item: Item,
    text: String,
    /// Why the last Enter was refused, shown in place of the text.
    error: Option<&'static str>,
}
fn accepts(item: Item, c: char) -> bool {
    match item {
        Item::Host => c.is_ascii_digit(),
        _ => c.is_ascii_alphanumeric() || ".-_:[]".contains(c),
    }
}
/// The action a typed value starts, or why it cannot.
fn commit(item: Item, text: &str) -> Result<Action, &'static str> {
    match item {
        Item::Host => crate::cli::parse_host_port(text)
            .map(Action::Host)
            .ok_or("PORT 1-65535"),
        _ => crate::cli::parse_join(text)
            .map(|address| Action::Join(address.to_owned()))
            .ok_or("NEEDS HOST:PORT"),
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
    /// Save this COM port for the next launch.
    Com(u8),
    /// Listen on this TCP port.
    Host(u16),
    /// Dial this `host:port`.
    Join(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Main,
    Modem,
    Peers,
}
impl Page {
    fn fixed_items(self) -> Option<&'static [Item]> {
        match self {
            Self::Main => Some(&ITEMS),
            Self::Modem => Some(&MODEM_ITEMS),
            Self::Peers => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PeerKey {
    id: u64,
    ip: IpAddr,
}
impl PeerKey {
    fn of(peer: &Peer) -> Self {
        Self {
            id: peer.id,
            ip: peer.address.ip(),
        }
    }
    fn matches(&self, peer: &Peer) -> bool {
        self.id == peer.id && self.ip == peer.address.ip()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum PeerSelection {
    #[default]
    None,
    Selected(PeerKey),
    Missing(PeerKey),
}

#[derive(Default)]
pub struct Menu {
    open: bool,
    page: Page,
    selected: usize,
    edit: Option<Edit>,
    peers: Vec<Peer>,
    peer_selection: PeerSelection,
}
impl Menu {
    pub fn is_open(&self) -> bool {
        self.open
    }
    /// Opens on the first row, or closes. Returns the new state.
    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        self.page = Page::Main;
        self.selected = 0;
        self.edit = None;
        self.open
    }

    /// Replaces the latest discovery snapshot without changing a selected
    /// host unless its identity has disappeared.
    pub fn update_peers(&mut self, peers: &[Peer]) {
        self.peers = peers.iter().take(PEER_LIMIT).cloned().collect();
        let missing = match &self.peer_selection {
            PeerSelection::Selected(key) if !self.peers.iter().any(|peer| key.matches(peer)) => {
                Some(key.clone())
            }
            _ => None,
        };
        if let Some(key) = missing {
            self.peer_selection = PeerSelection::Missing(key);
        }
        if self.page == Page::Peers
            && self.peer_selection == PeerSelection::None
            && !self.peers.is_empty()
        {
            self.peer_selection = PeerSelection::Selected(PeerKey::of(&self.peers[0]));
        }
    }

    /// The key help under the rows.
    fn hint(&self) -> &'static str {
        if self.editing() {
            EDIT_HINT
        } else {
            match self.page {
                Page::Main => HINT,
                Page::Modem => MODEM_HINT,
                Page::Peers if self.peers.is_empty() => PEERS_EMPTY_HINT,
                Page::Peers => PEERS_HINT,
            }
        }
    }
    /// A value is being typed: keys edit it rather than move.
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }
    pub fn selected(&self) -> Item {
        match self.page {
            Page::Peers if self.selected_peer().is_some() => Item::Peer,
            Page::Peers => Item::PeerStatus,
            page => page.fixed_items().expect("fixed menu page")[self.selected],
        }
    }
    fn selected_peer(&self) -> Option<&Peer> {
        let PeerSelection::Selected(key) = &self.peer_selection else {
            return None;
        };
        self.peers.iter().find(|peer| key.matches(peer))
    }
    fn selected_peer_index(&self) -> Option<usize> {
        let PeerSelection::Selected(key) = &self.peer_selection else {
            return None;
        };
        self.peers.iter().position(|peer| key.matches(peer))
    }
    fn visible_peer_start(&self) -> usize {
        self.selected_peer_index()
            .map_or(0, |index| (index / PEER_PAGE_SIZE) * PEER_PAGE_SIZE)
    }
    fn visible_row_count(&self) -> usize {
        if self.page != Page::Peers {
            return self.page.fixed_items().expect("fixed menu page").len();
        }
        if self.peers.is_empty() {
            1
        } else {
            self.peers
                .len()
                .saturating_sub(self.visible_peer_start())
                .min(PEER_PAGE_SIZE)
        }
    }
    fn enter_peers(&mut self) {
        self.page = Page::Peers;
        self.selected = 0;
        if self.peer_selection == PeerSelection::None {
            if let Some(peer) = self.peers.first() {
                self.peer_selection = PeerSelection::Selected(PeerKey::of(peer));
            }
        }
    }
    /// Applies one key press. `Close` returns from the modem submenu or closes
    /// the main menu.
    pub fn handle(&mut self, key: Key, values: &Values) -> Action {
        if !self.open {
            return Action::None;
        }
        if let Some(edit) = &mut self.edit {
            return match key {
                Key::Char(c) if accepts(edit.item, c) && edit.text.len() < EDIT_MAX => {
                    edit.text.push(c);
                    edit.error = None;
                    Action::None
                }
                Key::Backspace => {
                    edit.text.pop();
                    edit.error = None;
                    Action::None
                }
                Key::Select => match commit(edit.item, &edit.text) {
                    Ok(action) => {
                        self.edit = None;
                        action
                    }
                    Err(error) => {
                        edit.error = Some(error);
                        Action::None
                    }
                },
                // Esc cancels the typing, not the menu.
                Key::Close => {
                    self.edit = None;
                    Action::None
                }
                _ => Action::None,
            };
        }
        if self.page == Page::Peers {
            return self.handle_peer_keys(key, values);
        }
        let item = self.selected();
        if self.page == Page::Main && item == Item::Modem && matches!(key, Key::Select | Key::Right)
        {
            self.page = Page::Modem;
            self.selected = 0;
            return Action::None;
        }
        if self.page == Page::Modem
            && item == Item::LanHosts
            && matches!(key, Key::Select | Key::Right)
        {
            self.enter_peers();
            return Action::None;
        }
        if matches!(item, Item::Host | Item::Join) {
            if values.modem.is_active() {
                return match key {
                    Key::Up | Key::Down | Key::Left | Key::Close => {
                        self.handle_row_keys(key, values)
                    }
                    _ => Action::None,
                };
            }
            let shown = match item {
                Item::Host => values.host_port.to_string(),
                _ => values.join.clone(),
            };
            let mut begin = |text: String| {
                self.edit = Some(Edit {
                    item,
                    text,
                    error: None,
                });
                Action::None
            };
            match key {
                // Enter starts the shown value, or asks for one if none is usable.
                Key::Select => {
                    return commit(item, &shown).unwrap_or_else(|_| begin(String::new()));
                }
                Key::Backspace => {
                    let mut text = shown;
                    text.pop();
                    return begin(text);
                }
                Key::Char(c) if accepts(item, c) => return begin(c.to_string()),
                Key::Left => return self.handle_row_keys(key, values),
                Key::Right | Key::Char(_) => return Action::None,
                _ => {}
            }
        }
        self.handle_row_keys(key, values)
    }
    fn handle_peer_keys(&mut self, key: Key, values: &Values) -> Action {
        match key {
            Key::Up => self.move_peer_selection(false),
            Key::Down => self.move_peer_selection(true),
            Key::Left | Key::Close => {
                self.page = Page::Modem;
                self.selected = MODEM_ITEMS
                    .iter()
                    .position(|item| *item == Item::LanHosts)
                    .expect("LAN HOSTS row");
            }
            Key::Select if !values.modem.is_active() => {
                return self
                    .selected_peer()
                    .map_or(Action::None, |peer| Action::Join(peer.address.to_string()));
            }
            Key::Select | Key::Right | Key::Char(_) | Key::Backspace => return Action::None,
        }
        Action::None
    }
    fn move_peer_selection(&mut self, forward: bool) {
        if self.peers.is_empty() {
            return;
        }
        let next = match self.selected_peer_index() {
            Some(index) if forward => (index + 1) % self.peers.len(),
            Some(index) => (index + self.peers.len() - 1) % self.peers.len(),
            None if forward => 0,
            None => self.peers.len() - 1,
        };
        self.peer_selection = PeerSelection::Selected(PeerKey::of(&self.peers[next]));
    }
    fn handle_row_keys(&mut self, key: Key, values: &Values) -> Action {
        let items = self.page.fixed_items().expect("fixed menu page");
        let action = match key {
            Key::Up => {
                let count = items.len();
                self.selected = (self.selected + count - 1) % count;
                Action::None
            }
            Key::Down => {
                self.selected = (self.selected + 1) % items.len();
                Action::None
            }
            Key::Close if self.page == Page::Modem => {
                self.page = Page::Main;
                self.selected = ITEMS.len() - 1;
                Action::None
            }
            Key::Close => Action::Close,
            Key::Char(_) | Key::Backspace => Action::None,
            Key::Left if self.page == Page::Modem => {
                self.page = Page::Main;
                self.selected = ITEMS.len() - 1;
                Action::None
            }
            Key::Left => self.step(false, values),
            Key::Right => self.step(true, values),
            Key::Select => match self.selected() {
                Item::Recording => Action::Record(values.recording.is_none()),
                _ => self.step(true, values),
            },
        };
        if action == Action::Close {
            self.open = false;
            self.page = Page::Main;
            self.selected = 0;
            self.edit = None;
        }
        action
    }
    fn step(&self, forward: bool, values: &Values) -> Action {
        match self.selected() {
            Item::Scaler => Action::Scaler(values.scaler.cycle(forward)),
            Item::Fullscreen => Action::Fullscreen(!values.fullscreen),
            Item::Mute if values.mute_locked => Action::None,
            Item::Mute => Action::Mute(!values.mute),
            Item::Com => Action::Com(if values.next_com == 1 { 2 } else { 1 }),
            Item::Recording
            | Item::Modem
            | Item::LanHosts
            | Item::Line
            | Item::Host
            | Item::Join
            | Item::Peer
            | Item::PeerStatus => Action::None,
        }
    }
    /// Dims the frame and draws the menu over it; does nothing when closed.
    pub fn render(&self, buffer: &mut [u32], width: usize, height: usize, values: &Values) {
        if !self.open || buffer.len() < width * height {
            return;
        }
        let buffer = &mut buffer[..width * height];
        dim(buffer);
        let layout = layout(width, height, self.visible_row_count());
        let mut canvas = Canvas {
            buffer,
            width,
            height,
            scale: layout.scale,
        };
        let line = GLYPH_H * layout.scale;
        let mut y = layout.y;
        let title = self.title(values);
        canvas.text(layout.x, y, title.as_bytes(), WHITE);
        y += 2 * line;
        match self.page {
            Page::Peers => {
                self.render_peers(&mut canvas, layout.x, y, line, layout.pitch, values);
                y += self.visible_row_count() * layout.pitch * line;
            }
            page => {
                for (index, &item) in page
                    .fixed_items()
                    .expect("fixed menu page")
                    .iter()
                    .enumerate()
                {
                    let selected = index == self.selected;
                    let value = self
                        .edit
                        .as_ref()
                        .filter(|edit| edit.item == item)
                        .map_or_else(|| value_text(item, values), edit_text);
                    let locked = match item {
                        Item::Mute => values.mute_locked,
                        Item::Host | Item::Join => values.modem.is_active(),
                        _ => false,
                    };
                    draw_row(&mut canvas, layout.x, y, item, &value, selected, locked);
                    y += layout.pitch * line;
                }
            }
        }
        // One blank row after the last item, then the hint.
        let hint = y + 2 * line - layout.pitch * line;
        canvas.text(layout.x, hint, self.hint().as_bytes(), GREY);
    }
    fn title(&self, values: &Values) -> String {
        match self.page {
            Page::Main if values.pauses() => "gp64  PAUSED".into(),
            Page::Main => "gp64  RUNNING (MODEM IN USE)".into(),
            Page::Modem => "gp64  MODEM SETUP".into(),
            Page::Peers if matches!(&self.peer_selection, PeerSelection::Missing(_)) => {
                "gp64  HOST LOST - PICK AGAIN".into()
            }
            Page::Peers if self.peers.is_empty() && values.discovery_error.is_some() => {
                "gp64  DISCOVERY ERROR".into()
            }
            Page::Peers if self.peers.is_empty() => "gp64  SEARCHING LAN HOSTS".into(),
            Page::Peers if self.selected_peer().is_none() => "gp64  PICK A LAN HOST".into(),
            Page::Peers => {
                let page = self.visible_peer_start() / PEER_PAGE_SIZE + 1;
                let pages = self.peers.len().div_ceil(PEER_PAGE_SIZE);
                format!("gp64  LAN HOSTS {page}/{pages}")
            }
        }
    }
    fn render_peers(
        &self,
        canvas: &mut Canvas<'_>,
        x: usize,
        mut y: usize,
        line: usize,
        pitch: usize,
        values: &Values,
    ) {
        if self.peers.is_empty() {
            let value = peer_status_text(self, values);
            draw_row(canvas, x, y, Item::PeerStatus, &value, true, true);
            return;
        }
        let start = self.visible_peer_start();
        let end = (start + PEER_PAGE_SIZE).min(self.peers.len());
        for index in start..end {
            let peer = &self.peers[index];
            let selected = self.selected_peer_index() == Some(index);
            draw_row(
                canvas,
                x,
                y,
                Item::Peer,
                &peer_text(peer),
                selected,
                values.modem.is_active(),
            );
            y += pitch * line;
        }
    }
}

/// A value being typed, with a cursor; its tail when it is too long, or
/// the reason Enter refused it.
fn edit_text(edit: &Edit) -> String {
    if let Some(error) = edit.error {
        return error.into();
    }
    let text = format!("{}_", edit.text.to_ascii_uppercase());
    let skip = text.len().saturating_sub(VALUE_COLS);
    text[skip..].into()
}

fn draw_row(
    canvas: &mut Canvas<'_>,
    x: usize,
    y: usize,
    item: Item,
    value: &str,
    selected: bool,
    locked: bool,
) {
    let mut row = vec![if selected { MARKER } else { b' ' }, b' '];
    row.extend_from_slice(item.label().as_bytes());
    if !value.is_empty() {
        row.resize(LABEL_COLS, b' ');
        row.extend_from_slice(value.as_bytes());
    }
    let colour = match (selected, locked) {
        (_, true) => GREY,
        (true, false) => YELLOW,
        (false, false) => WHITE,
    };
    canvas.text(x, y, &row, colour);
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

fn error_value(error: &str) -> String {
    ascii_uppercase(&format!("ERROR: {error}"), VALUE_COLS)
}

fn peer_status_text(menu: &Menu, values: &Values) -> String {
    if matches!(&menu.peer_selection, PeerSelection::Missing(_)) {
        "HOST REMOVED".into()
    } else if let Some(error) = &values.discovery_error {
        error_value(error)
    } else {
        "SEARCHING...".into()
    }
}

fn peer_address_text(peer: &Peer) -> String {
    let full = peer.address.to_string();
    if full.len() <= VALUE_COLS {
        return full;
    }
    let ip = peer.address.ip().to_string();
    let suffix = format!("]:{}", peer.address.port());
    let ip_budget = VALUE_COLS - suffix.len() - 2;
    let leading = ip_budget.div_ceil(2);
    let trailing = ip_budget - leading;
    format!(
        "[{}~{}{}",
        &ip[..leading],
        &ip[ip.len() - trailing..],
        suffix
    )
}

fn peer_text(peer: &Peer) -> String {
    let address = peer_address_text(peer);
    let name_room = VALUE_COLS.saturating_sub(address.len() + 1);
    if name_room == 0 {
        return address;
    }
    let name = ascii_uppercase(peer.name.trim(), name_room);
    if name.is_empty() {
        address
    } else {
        format!("{address} {name}")
    }
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
        Item::Modem => "SETUP".into(),
        Item::LanHosts => {
            if let Some(error) = &values.discovery_error {
                error_value(error)
            } else if values.peers.is_empty() {
                "SEARCHING...".into()
            } else if values.peers.len() == 1 {
                "1 HOST".into()
            } else {
                format!("{} HOSTS", values.peers.len().min(PEER_LIMIT))
            }
        }
        Item::Line => line_text(&values.modem),
        Item::Com if values.next_com != values.com => {
            format!("COM{} (COM{} NEXT RUN)", values.com, values.next_com)
        }
        Item::Com => format!("COM{}", values.com),
        Item::Host => values.host_port.to_string(),
        Item::Join if values.join.is_empty() => "TYPE HOST:PORT".into(),
        Item::Join => {
            let text = values.join.to_ascii_uppercase();
            let skip = text.len().saturating_sub(VALUE_COLS);
            text[skip..].into()
        }
        Item::Peer | Item::PeerStatus => String::new(),
    }
}

/// Glyphs available for a row's value.
const VALUE_COLS: usize = BLOCK_COLS - LABEL_COLS;
/// The line status in one row: the full sentence when it fits, else its
/// leading words.
fn line_text(status: &LineStatus) -> String {
    let full = status.to_string().to_ascii_uppercase();
    if full.len() <= VALUE_COLS {
        return full;
    }
    let short = match status {
        LineStatus::Connected(peer) => {
            // The peer's port is ephemeral; its address is what matters.
            let host = peer
                .rsplit_once(':')
                .map_or(peer.as_str(), |(host, _)| host);
            let text = format!("CONNECTED {host}");
            if text.len() <= VALUE_COLS {
                return text;
            }
            "CONNECTED".into()
        }
        // The address being dialled is on its own row.
        LineStatus::Connecting(_) => "CONNECTING".into(),
        _ => full,
    };
    short.chars().take(VALUE_COLS).collect()
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
    let mut text = vec![BULLET];
    text.extend_from_slice(format!(" REC {}", clock(ms)).as_bytes());
    // One scale step per 360 window rows, up to the menu's largest.
    let scale = (height / 360).clamp(1, SCALE_MAX);
    let text_w = text.len() * GLYPH_W * scale;
    // Raised cosine: bright at the start of each cycle, dim half-way.
    let phase = (pulse_ms % PULSE_MS) as f64 / PULSE_MS as f64;
    let t = (1.0 + (phase * std::f64::consts::TAU).cos()) / 2.0;
    let red = REC_DIM + (t * f64::from(REC_BRIGHT - REC_DIM)).round() as u32;
    let mut canvas = Canvas {
        buffer: &mut buffer[..width * height],
        width,
        height,
        scale,
    };
    let x = width.saturating_sub(text_w + MARGIN * scale);
    canvas.text(x, MARGIN * scale, &text, red << 16);
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
    scale: usize,
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
                    if py < 0 || py as usize >= self.height {
                        continue;
                    }
                    for sx in 0..scale {
                        let px = x + column as isize * scale + sx;
                        if px >= 0 && (px as usize) < self.width {
                            self.buffer[py as usize * self.width + px as usize] = colour;
                        }
                    }
                }
            }
        }
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
            modem: LineStatus::Off,
            com: 2,
            next_com: 2,
            host_port: 23456,
            join: String::new(),
            peers: Vec::new(),
            discovery_error: None,
        }
    }
    fn open() -> Menu {
        let mut menu = Menu::default();
        assert!(menu.toggle());
        menu
    }
    fn select(menu: &mut Menu, item: Item) {
        assert!(
            menu.page
                .fixed_items()
                .is_some_and(|items| items.contains(&item)),
            "{item:?} is not visible"
        );
        while menu.selected() != item {
            menu.handle(Key::Down, &values());
        }
    }
    fn open_modem() -> Menu {
        let mut menu = open();
        select(&mut menu, Item::Modem);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Line);
        menu
    }
    fn peer(id: u64, name: &str, address: &str) -> Peer {
        Peer {
            id,
            name: name.into(),
            address: address.parse().expect("valid peer address"),
        }
    }
    fn modem_peers(menu: &mut Menu, peers: &[Peer]) {
        menu.update_peers(peers);
        select(menu, Item::LanHosts);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
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
    fn modem_submenu_opens_and_escape_or_left_returns_to_modem() {
        let mut menu = open();
        select(&mut menu, Item::Modem);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
        assert_ne!(menu.selected(), Item::Modem);
        assert_eq!(menu.handle(Key::Close, &values()), Action::None);
        assert!(menu.is_open());
        assert_eq!(menu.selected(), Item::Modem);

        assert_eq!(menu.handle(Key::Right, &values()), Action::None);
        assert_ne!(menu.selected(), Item::Modem);
        assert_eq!(menu.handle(Key::Left, &values()), Action::None);
        assert!(menu.is_open());
        assert_eq!(menu.selected(), Item::Modem);

        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
        assert!(!menu.toggle());
        assert!(menu.toggle());
        assert_eq!(menu.selected(), Item::Scaler);
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

        let mut menu = open_modem();
        assert_eq!(menu.handle(Key::Up, &values()), Action::None);
        assert_eq!(menu.selected(), Item::LanHosts);
        assert_eq!(menu.handle(Key::Down, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Line);
        for item in MODEM_ITEMS.into_iter().skip(1) {
            menu.handle(Key::Down, &values());
            assert_eq!(menu.selected(), item);
        }
    }

    #[test]
    fn main_value_rows_cycle_both_ways_and_emit_actions() {
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
    fn escape_closes_the_main_menu_and_f12_resets_the_submenu() {
        let mut menu = open();
        assert_eq!(menu.handle(Key::Close, &values()), Action::Close);
        assert!(!menu.is_open());
        assert_eq!(menu.selected(), Item::Scaler);

        let mut menu = open_modem();
        assert!(!menu.toggle());
        assert!(!menu.is_open());
        assert!(menu.toggle());
        assert_eq!(menu.selected(), Item::Scaler);
    }

    #[test]
    fn main_and_modem_submenu_rows_are_grouped() {
        assert_eq!(
            ITEMS,
            [
                Item::Scaler,
                Item::Fullscreen,
                Item::Mute,
                Item::Recording,
                Item::Modem,
            ]
        );
        assert_eq!(
            MODEM_ITEMS,
            [
                Item::Line,
                Item::Com,
                Item::Host,
                Item::Join,
                Item::LanHosts,
            ]
        );
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
        let v = Values {
            mute_locked: true,
            recording: Some(999 * 60_000 + 59_999),
            ..values()
        };
        assert!(HINT.len() <= BLOCK_COLS && MODEM_HINT.len() <= BLOCK_COLS);
        assert!(PEERS_HINT.len() <= BLOCK_COLS && PEERS_EMPTY_HINT.len() <= BLOCK_COLS);
        assert!(EDIT_HINT.len() <= BLOCK_COLS);
        assert!("gp64  RUNNING (MODEM IN USE)".len() <= BLOCK_COLS);
        assert!("gp64  MODEM SETUP".len() <= BLOCK_COLS);
        for item in ITEMS.into_iter().chain(MODEM_ITEMS) {
            assert!(item.label().len() + 2 <= LABEL_COLS, "{item:?}");
            assert!(
                LABEL_COLS + value_text(item, &v).len() <= BLOCK_COLS,
                "{item:?}"
            );
        }
    }

    #[test]
    fn menu_never_overflows_the_window() {
        for rows in [1, 2, PEER_PAGE_SIZE, ITEMS.len(), MODEM_ITEMS.len()] {
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
    fn each_page_layout_uses_its_visible_row_count() {
        assert_eq!(layout(320, 200, ITEMS.len()).scale, 1);
        // At 640x400 the main menu gains a larger compact scale.
        assert_eq!(
            layout(640, 400, ITEMS.len()),
            layout_of(2, PITCH_COMPACT, 640, 400, ITEMS.len())
        );
        // The shorter main menu fits at scale 3 with roomy spacing.
        assert_eq!(
            layout(960, 720, ITEMS.len()),
            layout_of(3, PITCH_ROOMY, 960, 720, ITEMS.len())
        );
        assert_eq!(
            layout(1920, 1080, ITEMS.len()),
            layout_of(4, PITCH_ROOMY, 1920, 1080, ITEMS.len())
        );
        assert_eq!(
            layout(1920, 1080, MODEM_ITEMS.len()),
            layout_of(4, PITCH_ROOMY, 1920, 1080, MODEM_ITEMS.len())
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
        let main = open();
        let modem = open_modem();
        let mut peers = open_modem();
        let found = [
            peer(1, "desk", "10.0.0.1:23519"),
            peer(2, "laptop", "10.0.0.2:23519"),
            peer(3, "arcade", "10.0.0.3:23519"),
            peer(4, "server", "10.0.0.4:23519"),
            peer(5, "other", "10.0.0.5:23519"),
            peer(6, "sixth", "10.0.0.6:23519"),
        ];
        modem_peers(&mut peers, &found);
        for &(width, height) in WINDOWS {
            // A guard word after the buffer catches any write past the end.
            for menu in [&main, &modem, &peers] {
                let mut buffer = vec![0x0080_8080; width * height + 1];
                menu.render(&mut buffer[..width * height], width, height, &values());
                assert_eq!(buffer[width * height], 0x0080_8080, "{width}x{height}");
                if width >= 320 && height >= 200 {
                    assert!(buffer.contains(&YELLOW), "{width}x{height}");
                    assert!(buffer.contains(&WHITE), "{width}x{height}");
                    assert!(buffer.contains(&BLACK), "{width}x{height}");
                }
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
            scale: 3,
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

    #[test]
    fn modem_submenu_shows_its_back_hint_and_title() {
        let menu = open_modem();
        assert_eq!(menu.hint(), MODEM_HINT);
        let (width, height) = (960, 720);
        let mut buffer = vec![0; width * height];
        menu.render(&mut buffer, width, height, &values());
        assert_ne!(buffer, {
            let mut main = vec![0; width * height];
            open().render(&mut main, width, height, &values());
            main
        });
    }

    #[test]
    fn lan_hosts_value_reports_searching_count_and_discovery_error() {
        assert_eq!(value_text(Item::LanHosts, &values()), "SEARCHING...");
        let with_hosts = Values {
            peers: vec![peer(1, "desk", "10.0.0.1:23519")],
            ..values()
        };
        assert_eq!(value_text(Item::LanHosts, &with_hosts), "1 HOST");
        let with_two_hosts = Values {
            peers: vec![
                peer(1, "desk", "10.0.0.1:23519"),
                peer(2, "laptop", "10.0.0.2:23519"),
            ],
            ..values()
        };
        assert_eq!(value_text(Item::LanHosts, &with_two_hosts), "2 HOSTS");
        let failed = Values {
            discovery_error: Some("socket unavailable".into()),
            ..values()
        };
        assert_eq!(
            value_text(Item::LanHosts, &failed),
            "ERROR: SOCKET UNAVAILAB"
        );
    }

    #[test]
    fn empty_peer_page_is_read_only_and_returns_to_lan_hosts() {
        let mut menu = open_modem();
        select(&mut menu, Item::LanHosts);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
        assert_eq!(menu.selected(), Item::PeerStatus);
        assert_eq!(menu.hint(), PEERS_EMPTY_HINT);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);

        menu.update_peers(&[peer(3, "arcade", "10.0.0.3:23519")]);
        assert_eq!(menu.selected(), Item::Peer);
        assert_eq!(
            menu.handle(Key::Select, &values()),
            Action::Join("10.0.0.3:23519".into())
        );

        assert_eq!(menu.handle(Key::Left, &values()), Action::None);
        assert_eq!(menu.selected(), Item::LanHosts);
        assert_eq!(menu.handle(Key::Right, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Peer);
        assert_eq!(menu.handle(Key::Close, &values()), Action::None);
        assert_eq!(menu.selected(), Item::LanHosts);

        assert_eq!(menu.handle(Key::Right, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Peer);
        assert!(!menu.toggle());
        assert!(menu.toggle());
        assert_eq!(menu.selected(), Item::Scaler);
    }

    #[test]
    fn peer_selection_tracks_identity_across_reorder_and_uses_latest_port() {
        let desk = peer(1, "desk", "10.0.0.1:23519");
        let laptop = peer(2, "laptop", "10.0.0.2:23519");
        let mut menu = open_modem();
        modem_peers(&mut menu, &[desk.clone(), laptop]);
        assert_eq!(menu.selected_peer().map(|peer| peer.id), Some(desk.id));

        let moved_desk = peer(1, "desk", "10.0.0.1:30000");
        let reordered_laptop = peer(2, "laptop", "10.0.0.2:23519");
        menu.update_peers(&[reordered_laptop, moved_desk]);
        assert_eq!(menu.selected_peer().map(|peer| peer.id), Some(desk.id));
        assert_eq!(
            menu.handle(Key::Select, &values()),
            Action::Join("10.0.0.1:30000".into())
        );
    }

    #[test]
    fn removed_peer_cannot_dial_a_replacement_until_explicitly_moved() {
        let desk = peer(1, "desk", "10.0.0.1:23519");
        let laptop = peer(2, "laptop", "10.0.0.2:23519");
        let mut menu = open_modem();
        modem_peers(&mut menu, &[desk.clone(), laptop]);
        menu.handle(Key::Down, &values());
        menu.update_peers(std::slice::from_ref(&desk));

        assert_eq!(menu.selected(), Item::PeerStatus);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
        menu.update_peers(&[desk.clone(), peer(2, "laptop", "10.0.0.2:23519")]);
        assert_eq!(menu.selected(), Item::PeerStatus);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
        assert_eq!(menu.handle(Key::Down, &values()), Action::None);
        assert_eq!(menu.selected_peer().map(|peer| peer.id), Some(desk.id));
        assert_eq!(
            menu.handle(Key::Select, &values()),
            Action::Join("10.0.0.1:23519".into())
        );
    }

    #[test]
    fn peer_picker_paginates_five_rows_and_renders_inside_every_window() {
        let peers: Vec<_> = (1..=12)
            .map(|id| peer(id, "host", &format!("10.0.0.{}:23519", id)))
            .collect();
        let mut menu = open_modem();
        modem_peers(&mut menu, &peers);
        for _ in 0..5 {
            menu.handle(Key::Down, &values());
        }
        assert_eq!(menu.selected_peer_index(), Some(5));
        assert_eq!(menu.visible_peer_start(), 5);
        assert_eq!(menu.visible_row_count(), PEER_PAGE_SIZE);
        for _ in 0..6 {
            menu.handle(Key::Down, &values());
        }
        assert_eq!(menu.selected_peer_index(), Some(11));
        assert_eq!(menu.visible_peer_start(), 10);
        assert_eq!(menu.visible_row_count(), 2);

        for &(width, height) in WINDOWS {
            let mut buffer = vec![0x0080_8080; width * height + 1];
            menu.render(&mut buffer[..width * height], width, height, &values());
            assert_eq!(buffer[width * height], 0x0080_8080, "{width}x{height}");
            if width >= 320 && height >= 200 {
                assert!(buffer.contains(&YELLOW), "{width}x{height}");
                assert!(buffer.contains(&WHITE), "{width}x{height}");
            }
        }
    }

    #[test]
    fn peer_join_uses_exact_address_and_is_blocked_while_modem_is_busy() {
        let target = peer(7, "arcade", "192.168.1.23:54321");
        let mut menu = open_modem();
        modem_peers(&mut menu, &[target]);
        let busy = Values {
            modem: LineStatus::Listening(23456),
            ..values()
        };
        assert_eq!(menu.handle(Key::Select, &busy), Action::None);
        assert_eq!(
            menu.handle(Key::Select, &values()),
            Action::Join("192.168.1.23:54321".into())
        );
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

    #[test]
    fn title_shows_whether_the_guest_is_paused() {
        let (width, height) = (960, 720);
        let mut paused = vec![0; width * height];
        let mut running = vec![0; width * height];
        open().render(&mut paused, width, height, &values());
        let v = Values {
            modem: LineStatus::Listening(23456),
            ..values()
        };
        open().render(&mut running, width, height, &v);
        assert_ne!(paused, running);
    }

    #[test]
    fn the_menu_pauses_only_while_no_line_is_up() {
        for (status, pauses) in [
            (LineStatus::Off, true),
            (LineStatus::Failed("refused".into()), true),
            (LineStatus::Listening(1), false),
            (LineStatus::Connecting("h:1".into()), false),
            (LineStatus::Connected("10.0.0.2:4000".into()), false),
        ] {
            let v = Values {
                modem: status.clone(),
                ..values()
            };
            assert_eq!(v.pauses(), pauses, "{status:?}");
        }
    }

    #[test]
    fn line_row_shows_the_line_status() {
        let text = |status| {
            value_text(
                Item::Line,
                &Values {
                    modem: status,
                    ..values()
                },
            )
        };
        assert_eq!(text(LineStatus::Off), "OFF");
        assert_eq!(text(LineStatus::Listening(23456)), "LISTENING ON :23456");
        assert_eq!(
            text(LineStatus::Connected("10.0.0.2:4000".into())),
            "CONNECTED 10.0.0.2"
        );
        assert_eq!(
            text(LineStatus::Connected("192.168.100.200:65535".into())),
            "CONNECTED"
        );
        assert_eq!(
            text(LineStatus::Connecting("example.org:23".into())),
            "CONNECTING"
        );
        assert_eq!(
            text(LineStatus::Connecting("h:1".into())),
            "CONNECTING TO H:1"
        );
        assert_eq!(
            text(LineStatus::Failed("refused".into())),
            "FAILED: REFUSED"
        );
        for status in [
            LineStatus::Connecting("a-very-long-host-name.example:23456".into()),
            LineStatus::Connected("[2001:db8::1234:5678]:23456".into()),
            LineStatus::Failed("x".repeat(40)),
        ] {
            assert!(text(status).len() <= VALUE_COLS);
        }
    }

    #[test]
    fn line_row_is_read_only_and_modem_entry_is_available() {
        let mut menu = open_modem();
        assert_eq!(menu.selected(), Item::Line);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
        assert_eq!(menu.handle(Key::Right, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Line);
        assert_eq!(menu.handle(Key::Down, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Com);
        assert_eq!(menu.handle(Key::Left, &values()), Action::None);
        assert!(menu.is_open());
        assert_eq!(menu.selected(), Item::Modem);
        assert_eq!(menu.handle(Key::Select, &values()), Action::None);
        assert_eq!(menu.selected(), Item::Line);
    }

    fn press(menu: &mut Menu, keys: &str, v: &Values) {
        for c in keys.chars() {
            assert_eq!(menu.handle(Key::Char(c), v), Action::None);
        }
    }
    fn editing_text(menu: &Menu) -> Option<String> {
        menu.edit.as_ref().map(edit_text)
    }

    #[test]
    fn typed_keys_edit_and_menu_keys_move() {
        use KeyCode::*;
        assert_eq!(keys(Numpad8, Some("8"), false), [Key::Up]);
        assert_eq!(keys(Numpad8, Some("8"), true), [Key::Char('8')]);
        assert_eq!(keys(Space, Some(" "), false), [Key::Select]);
        assert_eq!(keys(Space, Some(" "), true), [Key::Char(' ')]);
        assert_eq!(keys(KeyA, Some("a"), false), [Key::Char('a')]);
        assert_eq!(keys(Backspace, Some("\u{8}"), true), [Key::Backspace]);
        assert_eq!(keys(Enter, Some("\r"), true), [Key::Select]);
        assert_eq!(keys(Escape, Some("\u{1b}"), true), [Key::Close]);
        assert_eq!(keys(ArrowUp, None, true), [Key::Up]);
        assert_eq!(keys(F1, None, false), []);
    }

    #[test]
    fn host_row_starts_the_shown_port_or_a_typed_one() {
        let v = values();
        let mut menu = open_modem();
        select(&mut menu, Item::Host);
        assert_eq!(menu.handle(Key::Select, &v), Action::Host(23456));
        assert!(menu.is_open() && !menu.editing());
        // Typing replaces the port; letters are refused.
        press(&mut menu, "4x0", &v);
        assert_eq!(editing_text(&menu).as_deref(), Some("40_"));
        menu.handle(Key::Backspace, &v);
        press(&mut menu, "01", &v);
        assert_eq!(menu.handle(Key::Select, &v), Action::Host(401));
        assert!(!menu.editing());
        // Backspace edits the shown port.
        menu.handle(Key::Backspace, &v);
        assert_eq!(editing_text(&menu).as_deref(), Some("2345_"));
        // A bad port is refused inline and stays editable.
        for _ in 0..4 {
            menu.handle(Key::Backspace, &v);
        }
        press(&mut menu, "0", &v);
        assert_eq!(menu.handle(Key::Select, &v), Action::None);
        assert_eq!(editing_text(&menu).as_deref(), Some("PORT 1-65535"));
        menu.handle(Key::Backspace, &v);
        assert_eq!(editing_text(&menu).as_deref(), Some("_"));
        press(&mut menu, "99999", &v);
        assert_eq!(menu.handle(Key::Select, &v), Action::None);
        // Esc cancels the typing only; arrows are ignored while typing.
        assert_eq!(menu.handle(Key::Down, &v), Action::None);
        assert_eq!(menu.handle(Key::Close, &v), Action::None);
        assert!(menu.is_open() && !menu.editing());
        assert_eq!(menu.selected(), Item::Host);
        assert_eq!(menu.handle(Key::Close, &v), Action::None);
        assert!(menu.is_open());
        assert_eq!(menu.selected(), Item::Modem);
    }

    #[test]
    fn join_row_asks_for_an_address_until_one_is_saved() {
        let mut menu = open_modem();
        select(&mut menu, Item::Join);
        let empty = values();
        assert_eq!(value_text(Item::Join, &empty), "TYPE HOST:PORT");
        assert_eq!(menu.handle(Key::Select, &empty), Action::None);
        assert!(menu.editing());
        press(&mut menu, "peer", &empty);
        assert_eq!(menu.handle(Key::Select, &empty), Action::None);
        assert_eq!(editing_text(&menu).as_deref(), Some("NEEDS HOST:PORT"));
        // Spaces and other characters a host name cannot hold are refused.
        press(&mut menu, " :23/", &empty);
        assert_eq!(editing_text(&menu).as_deref(), Some("PEER:23_"));
        assert_eq!(
            menu.handle(Key::Select, &empty),
            Action::Join("peer:23".into())
        );
        let saved = Values {
            join: "peer:23".into(),
            ..values()
        };
        assert_eq!(value_text(Item::Join, &saved), "PEER:23");
        assert_eq!(
            menu.handle(Key::Select, &saved),
            Action::Join("peer:23".into())
        );
        // A long address shows its tail.
        let long = Values {
            join: format!("{}.example:23456", "h".repeat(40)),
            ..values()
        };
        let shown = value_text(Item::Join, &long);
        assert!(shown.len() <= VALUE_COLS && shown.ends_with(".EXAMPLE:23456"));
    }

    #[test]
    fn host_and_join_rest_while_a_line_is_up() {
        let busy = Values {
            modem: LineStatus::Connecting("peer:23".into()),
            join: "peer:23".into(),
            ..values()
        };
        let mut menu = open_modem();
        select(&mut menu, Item::Host);
        for key in [Key::Select, Key::Char('1'), Key::Backspace, Key::Right] {
            assert_eq!(menu.handle(key, &busy), Action::None);
            assert!(!menu.editing());
        }
        assert_eq!(menu.handle(Key::Down, &busy), Action::None);
        assert_eq!(menu.selected(), Item::Join);
        assert_eq!(menu.handle(Key::Select, &busy), Action::None);
        assert_eq!(menu.handle(Key::Close, &busy), Action::None);
        assert!(menu.is_open());
        assert_eq!(menu.selected(), Item::Modem);
        assert_eq!(menu.handle(Key::Select, &busy), Action::None);
        for _ in 0..3 {
            menu.handle(Key::Down, &busy);
        }
        assert_eq!(menu.selected(), Item::Join);
        assert_eq!(menu.handle(Key::Left, &busy), Action::None);
        assert_eq!(menu.selected(), Item::Modem);
        // Closing the menu discards a value being typed.
        let mut menu = open_modem();
        select(&mut menu, Item::Host);
        menu.handle(Key::Char('1'), &values());
        menu.toggle();
        menu.toggle();
        assert!(!menu.editing());
    }

    #[test]
    fn typing_shows_the_edit_hint() {
        let (width, height) = (960, 720);
        let draw = |menu: &Menu| {
            let mut buffer = vec![0; width * height];
            menu.render(&mut buffer, width, height, &values());
            buffer
        };
        let mut menu = open_modem();
        select(&mut menu, Item::Host);
        assert_eq!(menu.hint(), MODEM_HINT);
        let idle = draw(&menu);
        menu.handle(Key::Backspace, &values());
        assert_eq!(menu.hint(), EDIT_HINT);
        assert_ne!(draw(&menu), idle);
        menu.handle(Key::Close, &values());
        assert_eq!(menu.hint(), MODEM_HINT);
        menu.handle(Key::Close, &values());
        assert_eq!(menu.hint(), HINT);
    }

    #[test]
    fn com_row_saves_the_other_port_for_the_next_run() {
        let mut menu = open_modem();
        select(&mut menu, Item::Com);
        assert_eq!(value_text(Item::Com, &values()), "COM2");
        assert_eq!(menu.handle(Key::Right, &values()), Action::Com(1));
        let changed = Values {
            next_com: 1,
            ..values()
        };
        assert_eq!(value_text(Item::Com, &changed), "COM2 (COM1 NEXT RUN)");
        assert_eq!(menu.handle(Key::Select, &changed), Action::Com(2));
        assert_eq!(menu.selected(), Item::Com);
        assert_eq!(menu.handle(Key::Left, &changed), Action::None);
        assert_eq!(menu.selected(), Item::Modem);
    }
}
