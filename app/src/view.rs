//! Building the tree.
//!
//! Denise has no layout engine, so this file is the layout: plain arithmetic
//! over the surface size, redone from scratch whenever the surface changes
//! size. Everything here runs at build time only; what changes from second to
//! second is `refresh.rs`'s business, and it reaches the nodes through
//! [`Nodes`].

use denise::{Radius, Rect, Role, Size};
use denise_ui::widgets::{
    Align, Badge, Button, Column, Panel, RadialProgress, Select, Slider, Table, Tabs, TextInput,
    Toggle,
};
use denise_ui::{FontId, NodeId, TextStyle, Ui};
use hansolo_core::{Config, ThemePreference, WorkSource};

use crate::app::Message;
use crate::widgets::{Bars, Chart, Dot, HeaderMap, Logo, Pills, Text};

pub const PAGES: [&str; 6] = ["Dashboard", "Hardware", "Work", "Shares", "Log", "Settings"];
pub const PAGE_SETTINGS: usize = 5;

pub const THEMES: [&str; 3] = ["System", "Dark (dim)", "Light"];
pub const SOURCES: [&str; 2] = ["Solo pool (Stratum)", "Bitcoin Core node"];
pub const PRESETS: [(&str, &str); 4] = [
    ("public-pool.io", "stratum+tcp://public-pool.io:21496"),
    ("solo.ckpool.org", "stratum+tcp://solo.ckpool.org:3333"),
    ("eusolo.ckpool.org", "stratum+tcp://eusolo.ckpool.org:3333"),
    ("Custom", ""),
];
pub const CPU_BACKENDS: [&str; 7] = [
    "Automatic",
    "scalar",
    "sha-ni",
    "armv8-sha2",
    "neon",
    "avx2",
    "avx512",
];

pub fn theme_index(preference: ThemePreference) -> usize {
    match preference {
        ThemePreference::System => 0,
        ThemePreference::Dark => 1,
        ThemePreference::Light => 2,
    }
}

/// Every text style the dashboard uses, scaled once.
#[derive(Clone, Copy)]
pub struct Styles {
    pub scale: f32,
    pub body: TextStyle,
    pub small: TextStyle,
    pub title: TextStyle,
    pub value: TextStyle,
    pub big: TextStyle,
    pub brand: TextStyle,
    pub mono: TextStyle,
}

impl Styles {
    pub fn new(scale: f32, bold: Option<FontId>, mono: Option<FontId>) -> Self {
        let px = |v: f32| (v * scale + 0.5) as u16;
        let regular = |size| TextStyle::built_in(px(size));
        let heavy = |size| TextStyle {
            font: bold.unwrap_or(FontId::DEFAULT),
            size_px: px(size),
        };
        Self {
            scale,
            body: regular(14.0),
            small: regular(12.0),
            title: heavy(14.0),
            value: heavy(30.0),
            big: heavy(22.0),
            brand: heavy(19.0),
            mono: TextStyle {
                font: mono.unwrap_or(FontId::DEFAULT),
                size_px: px(13.0),
            },
        }
    }

    /// A logical length in physical pixels.
    pub fn u(&self, v: i32) -> i32 {
        (v as f32 * self.scale).round() as i32
    }
}

/// The nodes `refresh.rs` writes into.
pub struct Nodes {
    pub container: NodeId,
    pub pages: Vec<NodeId>,
    pub tabs: NodeId,

    // header
    pub status_badge: NodeId,
    pub uptime: NodeId,
    pub theme_select: NodeId,
    pub start_stop: NodeId,
    // footer
    pub footer_dot: NodeId,
    pub footer: NodeId,
    pub footer_right: NodeId,

    // dashboard
    pub stat_values: [NodeId; 4],
    pub stat_subs: [NodeId; 4],
    pub chart: NodeId,
    pub chart_legend: NodeId,
    pub lottery_ring: NodeId,
    pub lottery_lines: [NodeId; 4],
    pub devices: NodeId,
    pub conn_dot: NodeId,
    pub conn_values: Vec<NodeId>,

    // hardware
    pub hw_values: Vec<NodeId>,
    pub hw_features: NodeId,
    pub strategy: NodeId,
    pub bars: NodeId,
    pub gpus: NodeId,
    pub asics: NodeId,
    pub detect: NodeId,

    // work
    pub job_values: Vec<NodeId>,
    pub coinbase: NodeId,

    // shares
    pub share_values: [NodeId; 5],
    pub shares: NodeId,

    // log
    pub log: NodeId,

    // settings
    pub form: Form,
}

pub struct Form {
    pub address: NodeId,
    pub worker: NodeId,
    pub source: NodeId,
    pub pool_group: NodeId,
    pub node_group: NodeId,
    pub preset: NodeId,
    pub url: NodeId,
    pub username: NodeId,
    pub password: NodeId,
    pub rpc_url: NodeId,
    pub rpc_user: NodeId,
    pub rpc_password: NodeId,
    pub cookie: NodeId,
    pub cpu: NodeId,
    pub threads: NodeId,
    pub low_priority: NodeId,
    pub cpu_backend: NodeId,
    pub gpu: NodeId,
    pub intensity: NodeId,
    pub intensity_label: NodeId,
    pub asic: NodeId,
    pub usb: NodeId,
    pub network_devices: NodeId,
    pub theme: NodeId,
    pub autostart: NodeId,
    pub message: NodeId,
}

/// What a card's inside looks like to whoever fills it.
struct Card {
    node: NodeId,
    /// Content box, relative to the card.
    inner: Rect,
}

fn card(ui: &mut Ui<Message>, st: &Styles, parent: NodeId, rect: Rect, title: &str) -> Card {
    let node = ui
        .add(
            parent,
            Panel {
                fill: Some(Role::Base100),
                border: Some(Role::Base300),
                border_width: 1,
                radius: Radius::Box,
                backdrop: false,
            },
            rect,
        )
        .expect("card");
    let pad = st.u(18);
    let title_h = st.u(20);
    let mut top = pad;
    if !title.is_empty() {
        ui.add(
            node,
            Text::new(title, st.title),
            Rect::new(pad, pad - st.u(2), rect.width - 2 * pad, title_h),
        );
        top += title_h + st.u(10);
    }
    Card {
        node,
        inner: Rect::new(pad, top, rect.width - 2 * pad, rect.height - top - pad),
    }
}

fn text(ui: &mut Ui<Message>, parent: NodeId, widget: Text, rect: Rect) -> NodeId {
    ui.add(parent, widget, rect).expect("text")
}

/// A label on the left, a value on the right, one row. Returns the value node.
#[allow(clippy::too_many_arguments)]
fn key_value(
    ui: &mut Ui<Message>,
    st: &Styles,
    parent: NodeId,
    x: i32,
    y: i32,
    width: i32,
    label: &str,
    mono: bool,
) -> NodeId {
    let h = st.u(22);
    let label_w = (width * 2 / 5).min(st.u(170));
    text(
        ui,
        parent,
        Text::new(label, st.body).muted(),
        Rect::new(x, y, label_w, h),
    );
    let style = if mono { st.mono } else { st.body };
    text(
        ui,
        parent,
        Text::new("—", style),
        Rect::new(x + label_w, y, width - label_w, h),
    )
}

/// Splits `width` into `n` columns with `gap` between them.
fn columns(x: i32, width: i32, n: i32, gap: i32) -> Vec<(i32, i32)> {
    let w = (width - gap * (n - 1)) / n;
    (0..n)
        .map(|i| {
            (
                x + i * (w + gap),
                if i == n - 1 { width - i * (w + gap) } else { w },
            )
        })
        .collect()
}

pub fn build(
    ui: &mut Ui<Message>,
    st: &Styles,
    draft: &Config,
    page: usize,
    windowed: bool,
) -> Nodes {
    let size: Size = ui.size();
    let (w, h) = (size.width as i32, size.height as i32);
    let root = ui.root();
    let u = |v| st.u(v);

    // The page ground, oversized so its rounded corners are off the surface.
    let r = u(24);
    let container = ui
        .add(
            root,
            Panel::filled(Role::Base200),
            Rect::new(-r, -r, w + 2 * r, h + 2 * r),
        )
        .expect("ground");
    let origin = |rect: Rect| rect.translate(r, r);

    // ---------------------------------------------------------------- header
    let header_h = u(64);
    ui.add(
        container,
        Panel {
            fill: Some(Role::Base100),
            border: Some(Role::Base300),
            border_width: 1,
            radius: Radius::Box,
            backdrop: false,
        },
        origin(Rect::new(-r, -r, w + 2 * r, header_h + r)),
    );
    let margin = u(20);
    ui.add(
        container,
        Logo::new(st.big),
        origin(Rect::new(margin, (header_h - u(38)) / 2, u(38), u(38))),
    );
    text(
        ui,
        container,
        Text::new("HanSolo", st.brand),
        origin(Rect::new(margin + u(50), u(12), u(220), u(24))),
    );
    text(
        ui,
        container,
        Text::new("Bitcoin solo lottery miner", st.small).muted(),
        origin(Rect::new(margin + u(50), u(35), u(220), u(18))),
    );

    let button_w = u(128);
    let field_h = ui.theme().metrics.size_field;
    let y_mid = (header_h - field_h) / 2;
    let start_stop = ui
        .add(
            container,
            Button::new("Start mining", Message::StartStop)
                .with_role(Role::Primary)
                .with_style(st.title),
            origin(Rect::new(w - margin - button_w, y_mid, button_w, field_h)),
        )
        .expect("start");
    let select_w = u(140);
    let theme_select = ui
        .add(
            container,
            Select::new(THEMES, Message::OpenTheme)
                .with_selected(Some(theme_index(draft.ui.theme)))
                .with_style(st.body),
            origin(Rect::new(
                w - margin - button_w - u(12) - select_w,
                y_mid,
                select_w,
                field_h,
            )),
        )
        .expect("theme");
    let right_edge = w - margin - button_w - u(24) - select_w;
    let badge_w = u(150);
    let status_badge = ui
        .add(
            container,
            Badge::new("Stopped")
                .with_role(Role::Neutral)
                .with_style(st.small),
            origin(Rect::new(
                right_edge - badge_w,
                (header_h - u(24)) / 2,
                badge_w,
                u(24),
            )),
        )
        .expect("badge");
    let uptime = text(
        ui,
        container,
        Text::new("", st.small).muted().align(Align::End),
        origin(Rect::new(
            right_edge - badge_w - u(170),
            (header_h - u(20)) / 2,
            u(160),
            u(20),
        )),
    );
    // The title block and the status share a narrow window; the uptime goes first.
    if right_edge - badge_w - u(170) < margin + u(270) {
        ui.set_visible(uptime, false);
    }

    // ---------------------------------------------------------------- tabs
    let tabs_y = header_h + u(14);
    let tabs = ui
        .add(
            container,
            Tabs::new(PAGES, Message::Tab)
                .with_selected(page)
                .with_style(st.body),
            origin(Rect::new(
                margin,
                tabs_y,
                (w - 2 * margin).min(u(720)),
                field_h,
            )),
        )
        .expect("tabs");

    // ---------------------------------------------------------------- footer
    let footer_h = u(30);
    ui.add(
        container,
        Panel {
            fill: Some(Role::Base100),
            border: Some(Role::Base300),
            border_width: 1,
            radius: Radius::Box,
            backdrop: false,
        },
        origin(Rect::new(-r, h - footer_h, w + 2 * r, footer_h + r)),
    );
    let footer_dot = ui
        .add(
            container,
            Dot::new(Role::Neutral),
            origin(Rect::new(
                margin,
                h - footer_h + (footer_h - u(12)) / 2,
                u(12),
                u(12),
            )),
        )
        .expect("dot");
    let footer = text(
        ui,
        container,
        Text::new("Not connected", st.small).muted(),
        origin(Rect::new(margin + u(20), h - footer_h, w / 2, footer_h)),
    );
    let footer_right = text(
        ui,
        container,
        Text::new("", st.small).muted().align(Align::End),
        origin(Rect::new(w / 2, h - footer_h, w / 2 - margin, footer_h)),
    );

    // ---------------------------------------------------------------- pages
    let content_y = tabs_y + field_h + u(14);
    let content = Rect::new(0, content_y, w, h - footer_h - content_y);
    let mut pages = Vec::new();
    for i in 0..PAGES.len() {
        let page_node = ui
            .add(container, Panel::bare(), origin(content))
            .expect("page");
        ui.set_visible(page_node, i == page);
        pages.push(page_node);
    }
    let inner_w = w - 2 * margin;
    let gap = u(16);

    // ------------------------------------------------ dashboard
    let dash = pages[0];
    let n = if inner_w >= u(1000) {
        4
    } else if inner_w >= u(520) {
        2
    } else {
        1
    };
    let stat_h = u(118);
    let titles = ["Hashrate", "Best share", "Shares", "Block odds today"];
    let mut stat_values = Vec::new();
    let mut stat_subs = Vec::new();
    let cols = columns(margin, inner_w, n, gap);
    for (i, title) in titles.iter().enumerate() {
        let (x, cw) = cols[i % n as usize];
        let y = (i as i32 / n) * (stat_h + gap);
        let c = card(ui, st, dash, Rect::new(x, y, cw, stat_h), "");
        text(
            ui,
            c.node,
            Text::new(*title, st.small).muted(),
            Rect::new(c.inner.x, c.inner.y - u(2), c.inner.width, u(18)),
        );
        stat_values.push(text(
            ui,
            c.node,
            Text::new("—", st.value),
            Rect::new(c.inner.x, c.inner.y + u(18), c.inner.width, u(40)),
        ));
        stat_subs.push(text(
            ui,
            c.node,
            Text::new("", st.small).muted(),
            Rect::new(c.inner.x, c.inner.y + u(62), c.inner.width, u(18)),
        ));
    }
    let mut y = ((titles.len() as i32 + n - 1) / n) * (stat_h + gap);

    let row_h = u(280);
    let side_w = (inner_w / 4).clamp(u(320), u(420));
    let (chart_rect, lottery_rect) = if inner_w >= u(860) {
        (
            Rect::new(margin, y, inner_w - side_w - gap, row_h),
            Rect::new(margin + inner_w - side_w, y, side_w, row_h),
        )
    } else {
        (
            Rect::new(margin, y, inner_w, row_h),
            Rect::new(margin, y + row_h + gap, inner_w, row_h),
        )
    };
    let c = card(ui, st, dash, chart_rect, "Hashrate, last 15 minutes");
    let chart_legend = text(
        ui,
        c.node,
        Text::new("", st.small).muted().align(Align::End),
        Rect::new(c.inner.x, u(16), c.inner.width, u(20)),
    );
    let chart = ui
        .add(c.node, Chart::new(st.small, 900), c.inner)
        .expect("chart");

    let c = card(ui, st, dash, lottery_rect, "Lottery ticket");
    let ring = (c.inner.height - u(8)).min(u(140)).min(c.inner.width / 2);
    let lottery_ring = ui
        .add(
            c.node,
            RadialProgress::new(0.0)
                .with_label("0%")
                .with_role(Role::Primary)
                .with_thickness(u(12))
                .with_style(st.big),
            Rect::new(c.inner.x, c.inner.y, ring, ring),
        )
        .expect("ring");
    let tx = c.inner.x + ring + u(18);
    let tw = c.inner.width - ring - u(18);
    let mut lottery_lines = Vec::new();
    let captions = [
        "Best share vs. block",
        "Zero bits",
        "Chance this year",
        "Expected wait",
    ];
    for (i, caption) in captions.iter().enumerate() {
        let ly = c.inner.y + i as i32 * u(36);
        text(
            ui,
            c.node,
            Text::new(*caption, st.small).muted(),
            Rect::new(tx, ly, tw, u(16)),
        );
        lottery_lines.push(text(
            ui,
            c.node,
            Text::new("—", st.title),
            Rect::new(tx, ly + u(16), tw, u(18)),
        ));
    }
    text(
        ui,
        c.node,
        Text::new(
            "The ring fills logarithmically: each share difficulty 10× higher moves it the same distance. A block needs it full.",
            st.small,
        )
        .muted()
        .wrapped(),
        Rect::new(c.inner.x, c.inner.y + ring + u(12), c.inner.width, c.inner.bottom() - c.inner.y - ring - u(12)),
    );
    y = lottery_rect.bottom() + gap;

    let dev_h = (content.height - y - gap).max(u(240));
    let conn_w = u(380);
    let (dev_rect, conn_rect) = if inner_w >= u(900) {
        (
            Rect::new(margin, y, inner_w - conn_w - gap, dev_h),
            Rect::new(margin + inner_w - conn_w, y, conn_w, dev_h),
        )
    } else {
        (
            Rect::new(margin, y, inner_w, dev_h),
            Rect::new(margin, y + dev_h + gap, inner_w, u(300)),
        )
    };
    let c = card(ui, st, dash, dev_rect, "Devices");
    let devices = ui
        .add(
            c.node,
            Table::<Message>::inert([
                Column::flex("Device"),
                Column::new("Path", u(190)),
                Column::new("Hashrate", u(110)).align_end(),
                Column::new("Temp", u(80)).align_center(),
                Column::new("   Status", u(170)),
            ])
            .with_style(st.body),
            c.inner,
        )
        .expect("devices");

    let c = card(ui, st, dash, conn_rect, "Connection");
    let conn_dot = ui
        .add(
            c.node,
            Dot::new(Role::Neutral),
            Rect::new(c.inner.right() - u(12), u(20), u(12), u(12)),
        )
        .expect("dot");
    let mut conn_values = Vec::new();
    for (i, label) in [
        "Source",
        "Server",
        "Worker",
        "Latency",
        "Share difficulty",
        "Network difficulty",
        "Block height",
        "Last work",
    ]
    .iter()
    .enumerate()
    {
        conn_values.push(key_value(
            ui,
            st,
            c.node,
            c.inner.x,
            c.inner.y + i as i32 * u(26),
            c.inner.width,
            label,
            false,
        ));
    }
    let dash_bottom = conn_rect.bottom().max(dev_rect.bottom()) + gap;
    if dash_bottom > content.height {
        ui.set_scrollable(dash, true);
    }

    // ------------------------------------------------ hardware
    let hw = pages[1];
    let two = inner_w >= u(900);
    let (left, right) = if two {
        let cols = columns(margin, inner_w, 2, gap);
        (cols[0], cols[1])
    } else {
        ((margin, inner_w), (margin, inner_w))
    };
    let sys_h = u(300);
    let c = card(
        ui,
        st,
        hw,
        Rect::new(left.0, 0, left.1, sys_h),
        "This machine",
    );
    let mut hw_values = Vec::new();
    for (i, label) in [
        "Operating system",
        "Architecture",
        "Processor",
        "Cores",
        "Memory",
    ]
    .iter()
    .enumerate()
    {
        hw_values.push(key_value(
            ui,
            st,
            c.node,
            c.inner.x,
            c.inner.y + i as i32 * u(26),
            c.inner.width,
            label,
            false,
        ));
    }
    let fy = c.inner.y + 5 * u(26) + u(10);
    text(
        ui,
        c.node,
        Text::new("SHA-256 relevant instruction sets", st.small).muted(),
        Rect::new(c.inner.x, fy, c.inner.width, u(18)),
    );
    let hw_features = ui
        .add(
            c.node,
            Pills::new(st.small),
            Rect::new(
                c.inner.x,
                fy + u(24),
                c.inner.width,
                c.inner.bottom() - fy - u(24),
            ),
        )
        .expect("pills");

    let strat_y = if two { 0 } else { sys_h + gap };
    let c = card(
        ui,
        st,
        hw,
        Rect::new(right.0, strat_y, right.1, sys_h),
        "Strategy",
    );
    let strategy = text(
        ui,
        c.node,
        Text::new(
            "Hardware is benchmarked when mining starts; the fastest path wins.",
            st.body,
        )
        .wrapped(),
        Rect::new(c.inner.x, c.inner.y, c.inner.width, u(64)),
    );
    let detect = ui
        .add(
            c.node,
            Button::new("Detect again", Message::Detect).with_style(st.body),
            Rect::new(c.inner.right() - u(130), u(12), u(130), u(30)),
        )
        .expect("detect");
    let bars = ui
        .add(
            c.node,
            Bars::new(st.small),
            Rect::new(
                c.inner.x,
                c.inner.y + u(74),
                c.inner.width,
                c.inner.height - u(74),
            ),
        )
        .expect("bars");

    let tables_y = strat_y + sys_h + gap;
    let (gpu_rect, asic_rect) = if two {
        (
            Rect::new(left.0, tables_y, left.1, u(220)),
            Rect::new(right.0, tables_y, right.1, u(220)),
        )
    } else {
        (
            Rect::new(margin, tables_y, inner_w, u(220)),
            Rect::new(margin, tables_y + u(220) + gap, inner_w, u(220)),
        )
    };
    let c = card(ui, st, hw, gpu_rect, "Graphics processors");
    let gpus = ui
        .add(
            c.node,
            Table::<Message>::inert([
                Column::flex("GPU"),
                Column::new("API", u(80)),
                Column::new("Use", u(160)),
            ])
            .with_style(st.body),
            c.inner,
        )
        .expect("gpus");
    let c = card(ui, st, hw, asic_rect, "ASIC miners");
    let asics = ui
        .add(
            c.node,
            Table::<Message>::inert([
                Column::flex("Device"),
                Column::new("Where", u(170)),
                Column::new("Status", u(160)),
            ])
            .with_style(st.body),
            c.inner,
        )
        .expect("asics");
    if asic_rect.bottom() + gap > content.height {
        ui.set_scrollable(hw, true);
    }

    // ------------------------------------------------ work
    let work = pages[2];
    let c = card(
        ui,
        st,
        work,
        Rect::new(margin, 0, inner_w, u(150)),
        "Anatomy of the header being hashed",
    );
    ui.add(
        c.node,
        HeaderMap::new(st.small, st.small),
        Rect::new(c.inner.x, c.inner.y, c.inner.width, u(60)),
    );
    text(
        ui,
        c.node,
        Text::new(
            "80 bytes, hashed twice with SHA-256. Only the 4-byte nonce changes per attempt; when all 4 billion are spent, the extranonce in the coinbase changes, which changes the merkle root and gives a fresh header.",
            st.small,
        )
        .muted()
        .wrapped(),
        Rect::new(c.inner.x, c.inner.y + u(62), c.inner.width, u(40)),
    );
    let job_rows = if inner_w >= u(900) { 7 } else { 14 };
    let job_h = u(66) + job_rows * u(26);
    let c = card(
        ui,
        st,
        work,
        Rect::new(margin, u(150) + gap, inner_w, job_h),
        "Current job",
    );
    let mut job_values = Vec::new();
    let job_labels: [(&str, bool); 14] = [
        ("Height", false),
        ("Job", true),
        ("Previous block", true),
        ("Version", true),
        ("Bits", true),
        ("Network target", true),
        ("Share target", true),
        ("Time", false),
        ("Merkle branch", false),
        ("Transactions", false),
        ("Reward", false),
        ("Received", false),
        ("Jobs received", false),
        ("Network difficulty", false),
    ];
    let kv_cols = if inner_w >= u(900) { 2 } else { 1 };
    let per_col = job_labels.len().div_ceil(kv_cols);
    let kcols = columns(c.inner.x, c.inner.width, kv_cols as i32, u(24));
    for (i, (label, mono)) in job_labels.iter().enumerate() {
        let (x, cw) = kcols[i / per_col];
        let row = (i % per_col) as i32;
        job_values.push(key_value(
            ui,
            st,
            c.node,
            x,
            c.inner.y + row * u(26),
            cw,
            label,
            *mono,
        ));
    }
    let cb_y = u(150) + gap + job_h + gap;
    let c = card(
        ui,
        st,
        work,
        Rect::new(margin, cb_y, inner_w, u(170)),
        "Coinbase transaction",
    );
    let coinbase = text(
        ui,
        c.node,
        Text::new("No work yet.", st.mono).wrapped(),
        c.inner,
    );
    if cb_y + u(170) + gap > content.height {
        ui.set_scrollable(work, true);
    }

    // ------------------------------------------------ shares
    let shares_page = pages[3];
    let n = if inner_w >= u(900) {
        5
    } else if inner_w >= u(520) {
        3
    } else {
        2
    };
    let cols = columns(margin, inner_w, n, gap);
    let mut share_values = Vec::new();
    let small_h = u(90);
    for (i, title) in ["Submitted", "Accepted", "Rejected", "Stale", "Blocks found"]
        .iter()
        .enumerate()
    {
        let (x, cw) = cols[i % n as usize];
        let y = (i as i32 / n) * (small_h + gap);
        let c = card(ui, st, shares_page, Rect::new(x, y, cw, small_h), "");
        text(
            ui,
            c.node,
            Text::new(*title, st.small).muted(),
            Rect::new(c.inner.x, c.inner.y - u(2), c.inner.width, u(18)),
        );
        share_values.push(text(
            ui,
            c.node,
            Text::new("0", st.big),
            Rect::new(c.inner.x, c.inner.y + u(20), c.inner.width, u(30)),
        ));
    }
    let ty = ((5 + n - 1) / n) * (small_h + gap);
    let c = card(
        ui,
        st,
        shares_page,
        Rect::new(margin, ty, inner_w, (content.height - ty - gap).max(u(260))),
        "Recent shares (UTC)",
    );
    let shares = ui
        .add(
            c.node,
            Table::<Message>::inert([
                Column::new("Time", u(100)),
                Column::flex("Device"),
                Column::new("Difficulty", u(130)).align_end(),
                Column::new("", u(24)),
                Column::new("Result", u(120)),
            ])
            .with_style(st.body),
            c.inner,
        )
        .expect("shares");

    // ------------------------------------------------ log
    let c = card(
        ui,
        st,
        pages[4],
        Rect::new(margin, 0, inner_w, content.height - gap),
        "Events, newest first (UTC)",
    );
    let log = ui
        .add(
            c.node,
            Table::<Message>::inert([
                Column::new("Time", u(100)),
                Column::new("Level", u(90)),
                Column::flex("Message"),
            ])
            .with_style(st.body),
            c.inner,
        )
        .expect("log");

    // ------------------------------------------------ settings
    let form = build_settings(
        ui,
        st,
        pages[PAGE_SETTINGS],
        draft,
        margin,
        inner_w,
        content.height,
        gap,
        windowed,
    );

    Nodes {
        container,
        pages,
        tabs,
        status_badge,
        uptime,
        theme_select,
        start_stop,
        footer_dot,
        footer,
        footer_right,
        stat_values: stat_values.try_into().expect("4"),
        stat_subs: stat_subs.try_into().expect("4"),
        chart,
        chart_legend,
        lottery_ring,
        lottery_lines: lottery_lines.try_into().expect("4"),
        devices,
        conn_dot,
        conn_values,
        hw_values,
        hw_features,
        strategy,
        bars,
        gpus,
        asics,
        detect,
        job_values,
        coinbase,
        share_values: share_values.try_into().expect("5"),
        shares,
        log,
        form,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_settings(
    ui: &mut Ui<Message>,
    st: &Styles,
    page: NodeId,
    draft: &Config,
    margin: i32,
    inner_w: i32,
    height: i32,
    gap: i32,
    windowed: bool,
) -> Form {
    let u = |v| st.u(v);
    let field_h = ui.theme().metrics.size_field;
    let row = field_h + u(34);
    let two = inner_w >= u(900);
    let cols = if two {
        columns(margin, inner_w, 2, gap)
    } else {
        vec![(margin, inner_w), (margin, inner_w)]
    };

    // A caption over a control, which is how daisyUI's `fieldset` reads.
    let field = |ui: &mut Ui<Message>, parent: NodeId, x: i32, y: i32, w: i32, caption: &str| {
        text(
            ui,
            parent,
            Text::new(caption, st.small).muted(),
            Rect::new(x, y, w, u(18)),
        );
        Rect::new(x, y + u(22), w, field_h)
    };
    let input = |ui: &mut Ui<Message>,
                 parent: NodeId,
                 rect: Rect,
                 value: &str,
                 placeholder: &str,
                 password: bool| {
        let mut widget = TextInput::<Message>::new()
            .with_placeholder(placeholder)
            .with_style(st.body)
            .with_password(password)
            .with_clipboard(Message::Clipboard);
        widget.set_text(value);
        ui.add(parent, widget, rect).expect("input")
    };

    // -------- payout
    let (lx, lw) = cols[0];
    let payout_h = u(58) + row * 2;
    let c = card(ui, st, page, Rect::new(lx, 0, lw, payout_h), "Payout");
    let r = field(
        ui,
        c.node,
        c.inner.x,
        c.inner.y,
        c.inner.width,
        "Bitcoin address that receives the block reward",
    );
    let address = input(ui, c.node, r, &draft.payout_address, "bc1q…", false);
    let r = field(
        ui,
        c.node,
        c.inner.x,
        c.inner.y + row,
        c.inner.width,
        "Worker name",
    );
    let worker = input(ui, c.node, r, &draft.worker_name, "hansolo", false);

    // -------- work source
    let source_y = payout_h + gap;
    let source_h = u(58) + row * 4;
    let c = card(
        ui,
        st,
        page,
        Rect::new(lx, source_y, lw, source_h),
        "Work source",
    );
    let is_node = matches!(draft.source, WorkSource::Node { .. });
    let half = columns(c.inner.x, c.inner.width, 2, u(12));
    let r = field(ui, c.node, half[0].0, c.inner.y, half[0].1, "Mode");
    let source = ui
        .add(
            c.node,
            Select::new(SOURCES, Message::OpenSource)
                .with_selected(Some(is_node as usize))
                .with_style(st.body),
            r,
        )
        .expect("source");

    let group_rect = Rect::new(0, c.inner.y + row, c.inner.right() + u(18), row * 3);
    let pool_group = ui.add(c.node, Panel::bare(), group_rect).expect("pool");
    let node_group = ui.add(c.node, Panel::bare(), group_rect).expect("node");
    ui.set_visible(pool_group, !is_node);
    ui.set_visible(node_group, is_node);
    let gx = c.inner.x;
    let gw = c.inner.width;
    let ghalf = columns(gx, gw, 2, u(12));

    let (url_value, user_value, pass_value) = match &draft.source {
        WorkSource::Stratum {
            url,
            username,
            password,
        } => (
            url.clone(),
            username.clone().unwrap_or_default(),
            password.clone(),
        ),
        WorkSource::Node { .. } => (WorkSource::default_url(), String::new(), "x".into()),
    };
    let preset_index = PRESETS
        .iter()
        .position(|(_, u)| *u == url_value)
        .unwrap_or(PRESETS.len() - 1);
    let r = field(ui, c.node, half[1].0, c.inner.y, half[1].1, "Pool");
    let preset = ui
        .add(
            c.node,
            Select::new(PRESETS.map(|(name, _)| name), Message::OpenPreset)
                .with_selected(Some(preset_index))
                .with_style(st.body),
            r,
        )
        .expect("preset");
    ui.set_visible(preset, !is_node);
    let r = field(ui, pool_group, gx, 0, gw, "Stratum URL");
    let url = input(
        ui,
        pool_group,
        r,
        &url_value,
        "stratum+tcp://host:port",
        false,
    );
    let r = field(
        ui,
        pool_group,
        ghalf[0].0,
        row,
        ghalf[0].1,
        "Username (blank: address.worker)",
    );
    let username = input(ui, pool_group, r, &user_value, "", false);
    let r = field(ui, pool_group, ghalf[1].0, row, ghalf[1].1, "Password");
    let password = input(ui, pool_group, r, &pass_value, "x", false);
    text(
        ui,
        pool_group,
        Text::new(
            "Solo pools pay the whole block to your address when you find it, minus a small fee.",
            st.small,
        )
        .muted()
        .wrapped(),
        Rect::new(gx, row * 2 + u(6), gw, u(40)),
    );

    let (rpc, rpc_u, rpc_p, cookie_value) = match &draft.source {
        WorkSource::Node {
            rpc_url,
            rpc_user,
            rpc_password,
            cookie_file,
            ..
        } => (
            rpc_url.clone(),
            rpc_user.clone(),
            rpc_password.clone(),
            cookie_file.clone().unwrap_or_default(),
        ),
        WorkSource::Stratum { .. } => (
            "http://127.0.0.1:8332".into(),
            String::new(),
            String::new(),
            String::new(),
        ),
    };
    let r = field(ui, node_group, gx, 0, gw, "RPC URL");
    let rpc_url = input(ui, node_group, r, &rpc, "http://127.0.0.1:8332", false);
    let r = field(ui, node_group, ghalf[0].0, row, ghalf[0].1, "RPC user");
    let rpc_user = input(ui, node_group, r, &rpc_u, "", false);
    let r = field(ui, node_group, ghalf[1].0, row, ghalf[1].1, "RPC password");
    let rpc_password = input(ui, node_group, r, &rpc_p, "", true);
    let r = field(
        ui,
        node_group,
        gx,
        row * 2,
        gw,
        "Cookie file (instead of user and password)",
    );
    let cookie = input(
        ui,
        node_group,
        r,
        &cookie_value,
        "~/.bitcoin/.cookie",
        false,
    );

    // -------- hardware
    let (rx, rw) = cols[1];
    let hw_y = if two { 0 } else { source_y + source_h + gap };
    let toggle_h = u(30);
    let hw_h = u(58) + toggle_h * 5 + row * 3 + u(20);
    let c = card(ui, st, page, Rect::new(rx, hw_y, rw, hw_h), "Hardware");
    let mut y = c.inner.y;
    let toggle = |ui: &mut Ui<Message>,
                  parent: NodeId,
                  y: i32,
                  label: &str,
                  on: bool,
                  message: fn(bool) -> Message| {
        ui.add(
            parent,
            Toggle::new(label, message)
                .with_checked(on)
                .with_style(st.body),
            Rect::new(c.inner.x, y, c.inner.width, toggle_h),
        )
        .expect("toggle")
    };
    let cpu = toggle(
        ui,
        c.node,
        y,
        "Mine on the CPU",
        draft.cpu.enabled,
        Message::CpuEnabled,
    );
    y += toggle_h + u(4);
    let low_priority = toggle(
        ui,
        c.node,
        y,
        "Low priority (keep the machine responsive)",
        draft.cpu.low_priority,
        Message::LowPriority,
    );
    y += toggle_h + u(8);
    let half = columns(c.inner.x, c.inner.width, 2, u(12));
    let r = field(
        ui,
        c.node,
        half[0].0,
        y,
        half[0].1,
        "Threads (blank: all cores)",
    );
    let threads = input(
        ui,
        c.node,
        r,
        &draft.cpu.threads.map(|t| t.to_string()).unwrap_or_default(),
        "auto",
        false,
    );
    let r = field(ui, c.node, half[1].0, y, half[1].1, "CPU hashing path");
    let backend_index = draft
        .cpu
        .backend
        .as_deref()
        .and_then(|b| CPU_BACKENDS.iter().position(|n| n.eq_ignore_ascii_case(b)))
        .unwrap_or(0);
    let cpu_backend = ui
        .add(
            c.node,
            Select::new(CPU_BACKENDS, Message::OpenCpuBackend)
                .with_selected(Some(backend_index))
                .with_style(st.body),
            r,
        )
        .expect("backend");
    y += row + u(4);
    let gpu = toggle(
        ui,
        c.node,
        y,
        "Mine on graphics processors",
        draft.gpu.enabled,
        Message::GpuEnabled,
    );
    y += toggle_h + u(4);
    let intensity_label = text(
        ui,
        c.node,
        Text::new(format!("GPU intensity {}", draft.gpu.intensity), st.small).muted(),
        Rect::new(c.inner.x, y, c.inner.width, u(18)),
    );
    let intensity = ui
        .add(
            c.node,
            Slider::new(1.0, 10.0, draft.gpu.intensity as f32, Message::Intensity).with_step(1.0),
            Rect::new(c.inner.x, y + u(20), c.inner.width, u(28)),
        )
        .expect("slider");
    y += u(56);
    let asic = toggle(
        ui,
        c.node,
        y,
        "Use ASIC miners",
        draft.asic.enabled,
        Message::AsicEnabled,
    );
    y += toggle_h + u(4);
    let usb = toggle(
        ui,
        c.node,
        y,
        "Probe USB serial ports",
        draft.asic.usb,
        Message::Usb,
    );
    y += toggle_h + u(8);
    let r = field(
        ui,
        c.node,
        c.inner.x,
        y,
        c.inner.width,
        "AxeOS network miners (Bitaxe, NerdQAxe), comma separated",
    );
    let network_devices = input(
        ui,
        c.node,
        r,
        &draft.asic.network_devices.join(", "),
        "192.168.1.50, bitaxe.local",
        false,
    );

    // -------- interface
    let ui_y = hw_y + hw_h + gap;
    let ui_x = if two { rx } else { lx };
    let ui_w = if two { rw } else { lw };
    let ui_h = u(58) + row + toggle_h + u(40);
    let c = card(ui, st, page, Rect::new(ui_x, ui_y, ui_w, ui_h), "Interface");
    let r = field(
        ui,
        c.node,
        c.inner.x,
        c.inner.y,
        c.inner.width.min(u(260)),
        if windowed {
            "Theme"
        } else {
            "Theme (System is dark on a bare display)"
        },
    );
    let theme = ui
        .add(
            c.node,
            Select::new(THEMES, Message::OpenTheme)
                .with_selected(Some(theme_index(draft.ui.theme)))
                .with_style(st.body),
            r,
        )
        .expect("theme");
    let autostart = ui
        .add(
            c.node,
            Toggle::new("Start mining when HanSolo opens", Message::Autostart)
                .with_checked(draft.autostart)
                .with_style(st.body),
            Rect::new(c.inner.x, c.inner.y + row, c.inner.width, toggle_h),
        )
        .expect("autostart");

    // -------- actions, under whichever column is longer
    let left_bottom = source_y + source_h;
    let right_bottom = ui_y + ui_h;
    let actions_y = if two {
        left_bottom.max(right_bottom)
    } else {
        right_bottom
    } + gap;
    let bw = u(150);
    ui.add(
        page,
        Button::new("Save", Message::Save)
            .with_role(Role::Primary)
            .with_style(st.title),
        Rect::new(margin, actions_y, bw, field_h),
    );
    ui.add(
        page,
        Button::new("Save and restart", Message::SaveRestart)
            .with_role(Role::Secondary)
            .with_style(st.title),
        Rect::new(margin + bw + u(12), actions_y, bw + u(20), field_h),
    );
    ui.add(
        page,
        Button::new("Revert", Message::Revert)
            .with_role(Role::Neutral)
            .with_style(st.body),
        Rect::new(margin + 2 * bw + u(44), actions_y, u(110), field_h),
    );
    let message = text(
        ui,
        page,
        Text::new("", st.body),
        Rect::new(
            margin + 2 * bw + u(170),
            actions_y,
            inner_w - 2 * bw - u(170),
            field_h,
        ),
    );
    if actions_y + field_h + gap > height {
        ui.set_scrollable(page, true);
    }

    Form {
        address,
        worker,
        source,
        pool_group,
        node_group,
        preset,
        url,
        username,
        password,
        rpc_url,
        rpc_user,
        rpc_password,
        cookie,
        cpu,
        threads,
        low_priority,
        cpu_backend,
        gpu,
        intensity,
        intensity_label,
        asic,
        usb,
        network_devices,
        theme,
        autostart,
        message,
    }
}

trait DefaultUrl {
    fn default_url() -> String;
}

impl DefaultUrl for WorkSource {
    fn default_url() -> String {
        PRESETS[0].1.to_string()
    }
}
