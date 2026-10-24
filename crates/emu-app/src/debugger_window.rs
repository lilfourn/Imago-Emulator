//! The debugger window (UI-12): execution control, disassembly with
//! breakpoints, registers, a memory editor, and a panel below for
//! breakpoints and watchpoints, graphics, I/O registers and the scheduler.
//!
//! The window keeps only what the user chose (the CPU, where memory and
//! code are shown, the breakpoint lists); everything it shows comes from
//! the emulation thread's latest [`Snapshot`], which the main window's
//! [`EmulatorView`] hands over. It redraws whenever the main window does,
//! which is after every frame.

use std::sync::Arc;

use emu_core::System;
use emu_core::debug::{Breakpoint, GraphicsRequest, GraphicsView, Stop, WatchKind, Watchpoint};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, KeyDownEvent, MouseButton, ObjectFit,
    RenderImage, ScrollWheelEvent, SharedString, Subscription, TitlebarOptions, WeakEntity, Window,
    WindowBounds, WindowHandle, WindowOptions, div, img, prelude::*, px, rgb, size,
};

use crate::app::{DebugNextFrame, DebugStep, DebugStepOver, TogglePause};
use crate::debugger::{self, BYTES_PER_ROW, MEMORY_ROWS, Panel, RunState, Snapshot, View};
use crate::memory_viewer::{self, Cursor, Viewer};
use crate::video;
use crate::view::EmulatorView;

/// Height of a line of monospaced text, in logical pixels.
const LINE: f32 = 15.0;

const BACKGROUND: u32 = 0x001E_1E1E;
const PANEL: u32 = 0x0026_2626;
const TEXT: u32 = 0x00DD_DDDD;
const DIM: u32 = 0x0088_8888;
const ACCENT: u32 = 0x0026_4F78;
const BREAKPOINT: u32 = 0x00E5_3935;
const CHANGED: u32 = 0x00FF_D54F;

/// A hex field being typed into, and what it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    field: Field,
    text: String,
}

/// What a typed address does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// Shows memory from there.
    Memory,
    /// Shows code from there; empty follows the PC again.
    Code,
    /// Adds a breakpoint there.
    Breakpoint,
    /// Adds a watchpoint on an address or a range, `start-end`.
    Watchpoint,
    /// Draws tiles from there.
    TileBase,
}

impl Field {
    fn prompt(self) -> &'static str {
        match self {
            Self::Memory => "Go to address",
            Self::Code => "Show code at (empty: follow PC)",
            Self::Breakpoint => "Breakpoint at",
            Self::Watchpoint => "Watch address or start-end",
            Self::TileBase => "Tiles from",
        }
    }
}

/// The debugger window's root view.
pub struct DebuggerWindow {
    main: WeakEntity<EmulatorView>,
    focus: FocusHandle,
    /// The game last set up for, by session number.
    session: Option<u64>,
    /// What the emulation thread is asked to gather.
    view: View,
    breakpoints: Vec<Breakpoint>,
    watchpoints: Vec<Watchpoint>,
    /// Which accesses new watchpoints catch.
    watch_kind: WatchKind,
    memory: Viewer,
    /// The byte being edited.
    cursor: Option<Cursor>,
    entry: Option<Entry>,
    /// The last stop the window followed to its CPU.
    followed: Option<Stop>,
    /// The registers as of the last stop or pause, to mark changes.
    previous_registers: Vec<u32>,
    /// The graphics picture shown, and the frame it was built from.
    picture: Option<(emu_core::Frame, Arc<RenderImage>)>,
    /// Images no longer shown, to remove from GPUI's atlas.
    stale_images: Vec<Arc<RenderImage>>,
    _subscriptions: Vec<Subscription>,
}

impl DebuggerWindow {
    /// Opens a debugger window for the game in `main`.
    pub fn open(main: Entity<EmulatorView>, cx: &mut App) -> anyhow::Result<WindowHandle<Self>> {
        let bounds = Bounds::centered(None, size(px(1180.0), px(820.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Debugger".into()),
                ..TitlebarOptions::default()
            }),
            window_min_size: Some(size(px(900.0), px(560.0))),
            app_id: Some("ds-emulator".to_owned()),
            ..WindowOptions::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| Self::new(main, window, cx))
        })
    }

    /// The window for the game in `main`, redrawn whenever `main` is.
    pub fn new(main: Entity<EmulatorView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let subscriptions = vec![
            cx.observe(&main, |_, _, cx| cx.notify()),
            // The debugger goes when the game's window does.
            cx.observe_release_in(&main, window, |_, _, window, _| window.remove_window()),
            // Closing the window stops the gathering and the stops.
            cx.on_release(|this, cx| {
                if let Some(main) = this.main.upgrade() {
                    let main = main.read(cx);
                    main.send_debug(debugger::Command::View(None));
                    main.send_debug(debugger::Command::SetStops {
                        breakpoints: Vec::new(),
                        watchpoints: Vec::new(),
                    });
                }
            }),
        ];
        Self {
            main: main.downgrade(),
            focus,
            session: None,
            view: View::default(),
            breakpoints: Vec::new(),
            watchpoints: Vec::new(),
            watch_kind: WatchKind::Write,
            memory: Viewer::default(),
            cursor: None,
            entry: None,
            followed: None,
            previous_registers: Vec::new(),
            picture: None,
            stale_images: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn send(&self, command: debugger::Command, cx: &Context<Self>) {
        if let Some(main) = self.main.upgrade() {
            main.read(cx).send_debug(command);
        }
    }

    /// Changes what the thread gathers, and asks for it.
    fn change_view(&mut self, change: impl FnOnce(&mut View), cx: &mut Context<Self>) {
        change(&mut self.view);
        self.send(debugger::Command::View(Some(self.view)), cx);
        cx.notify();
    }

    fn send_stops(&self, cx: &Context<Self>) {
        self.send(
            debugger::Command::SetStops {
                breakpoints: self.breakpoints.clone(),
                watchpoints: self.watchpoints.clone(),
            },
            cx,
        );
    }

    /// The CPU the window shows, which steps run.
    pub fn cpu(&self) -> usize {
        self.view.cpu
    }

    /// Runs the shown CPU for one instruction.
    fn step(&mut self, cx: &mut Context<Self>) {
        self.send(debugger::Command::Step(self.view.cpu), cx);
    }

    /// Steps, running a call through to its return.
    fn step_over(&mut self, cx: &mut Context<Self>) {
        self.send(debugger::Command::StepOver(self.view.cpu), cx);
    }

    /// Runs to the end of the frame.
    fn next_frame(&mut self, cx: &mut Context<Self>) {
        self.send(debugger::Command::NextFrame, cx);
    }

    /// Pauses a running game, or continues a paused or stopped one.
    fn toggle_run(&mut self, running: bool, cx: &mut Context<Self>) {
        let command = if running {
            debugger::Command::Pause
        } else {
            debugger::Command::Continue
        };
        self.send(command, cx);
    }

    fn toggle_breakpoint(&mut self, address: u32, cx: &mut Context<Self>) {
        let breakpoint = Breakpoint {
            cpu: self.view.cpu,
            address,
        };
        if let Some(index) = self.breakpoints.iter().position(|b| *b == breakpoint) {
            self.breakpoints.remove(index);
        } else {
            self.breakpoints.push(breakpoint);
        }
        self.send_stops(cx);
        cx.notify();
    }

    fn start_entry(&mut self, field: Field, cx: &mut Context<Self>) {
        self.entry = Some(Entry {
            field,
            text: String::new(),
        });
        self.cursor = None;
        cx.notify();
    }

    /// Acts on the typed address.
    fn commit_entry(&mut self, system: System, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.take() else {
            return;
        };
        let (first, last) = match entry.text.split_once('-') {
            Some((first, last)) => (first, Some(last)),
            None => (entry.text.as_str(), None),
        };
        let start = memory_viewer::parse_address(first);
        match (entry.field, start) {
            (Field::Code, None) if entry.text.trim().is_empty() => {
                self.change_view(|view| view.code = None, cx);
            }
            (Field::Code, Some(address)) => self.change_view(|view| view.code = Some(address), cx),
            (Field::Memory, Some(address)) => {
                self.memory
                    .go_to(address, memory_viewer::last_address(system));
                let start = self.memory.address;
                self.change_view(|view| view.memory = start, cx);
            }
            (Field::Breakpoint, Some(address)) => {
                if !self.breakpoints.iter().any(|b| b.address == address) {
                    self.toggle_breakpoint(address, cx);
                }
            }
            (Field::Watchpoint, Some(start)) => {
                let end = last
                    .and_then(memory_viewer::parse_address)
                    .unwrap_or(start)
                    .max(start);
                self.watchpoints.push(Watchpoint {
                    cpu: self.view.cpu,
                    start,
                    end,
                    kind: self.watch_kind,
                });
                self.send_stops(cx);
            }
            (Field::TileBase, Some(address)) => {
                self.change_view(|view| view.graphics.base = address, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform || keystroke.modifiers.control {
            return;
        }
        let Some(system) = self.system(cx) else {
            return;
        };
        let key = keystroke.key.as_str();
        if self.entry.is_some() {
            let typed = keystroke.key_char.as_deref().unwrap_or(key);
            match key {
                "enter" => self.commit_entry(system, cx),
                "escape" => self.entry = None,
                "backspace" => {
                    if let Some(entry) = &mut self.entry {
                        entry.text.pop();
                    }
                }
                _ => {
                    let allowed = |c: char| c.is_ascii_hexdigit() || "xX$-".contains(c);
                    if let Some(entry) = &mut self.entry
                        && typed.chars().all(allowed)
                    {
                        entry.text.push_str(typed);
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(cursor) = self.cursor else {
            return;
        };
        let moved = match key {
            "escape" => {
                self.cursor = None;
                cx.notify();
                return;
            }
            "left" => cursor.moved(-1),
            "right" => cursor.moved(1),
            "up" => cursor.moved(-(BYTES_PER_ROW as i32)),
            "down" => cursor.moved(BYTES_PER_ROW as i32),
            _ => {
                let Some(digit) = memory_viewer::hex_digit(key) else {
                    return;
                };
                let mut typed = cursor;
                if let Some((address, value)) = typed.type_digit(digit) {
                    let cpu = self.view.cpu;
                    self.send(
                        debugger::Command::Write {
                            cpu,
                            address,
                            value,
                        },
                        cx,
                    );
                }
                typed
            }
        };
        self.cursor = Some(moved);
        // Keep the cursor on screen.
        let shown = BYTES_PER_ROW * MEMORY_ROWS;
        let offset = moved.address.wrapping_sub(self.memory.address);
        if offset >= shown {
            let last = memory_viewer::last_address(system);
            if moved.address < self.memory.address {
                self.memory.go_to(moved.address, last);
            } else {
                let top = moved.address.saturating_sub(shown - BYTES_PER_ROW);
                self.memory.go_to(top, last);
            }
            let start = self.memory.address;
            self.change_view(|view| view.memory = start, cx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn scroll_memory(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(system) = self.system(cx) else {
            return;
        };
        let rows = -f32::from(event.delta.pixel_delta(px(LINE)).y) / LINE;
        let before = self.memory.address;
        self.memory
            .scroll(rows, memory_viewer::last_address(system));
        if self.memory.address != before {
            let start = self.memory.address;
            self.change_view(|view| view.memory = start, cx);
        }
    }

    fn system(&self, cx: &Context<Self>) -> Option<System> {
        let main = self.main.upgrade()?;
        main.read(cx).debug_target().map(|target| target.system)
    }

    /// Sets the window up for a newly started game, or for a change of
    /// player or a stop on another CPU.
    fn follow(
        &mut self,
        target: &crate::view::DebugTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session != Some(target.session) {
            self.session = Some(target.session);
            self.breakpoints.clear();
            self.watchpoints.clear();
            self.cursor = None;
            self.followed = None;
            self.previous_registers.clear();
            let start = memory_viewer::home(target.system);
            self.memory
                .go_to(start, memory_viewer::last_address(target.system));
            self.view = View {
                memory: self.memory.address,
                graphics: GraphicsRequest {
                    base: tile_bases(target.system)
                        .first()
                        .map_or(0, |(_, base)| *base),
                    ..GraphicsRequest::default()
                },
                ..self.view
            };
            self.send(debugger::Command::View(Some(self.view)), cx);
            self.send_stops(cx);
            window.set_window_title(&format!("Debugger — {}", target.system.name()));
        }
        let Some(snapshot) = &target.snapshot else {
            return;
        };
        // A stop shows its CPU, once; the user can then pick another.
        if let RunState::Stopped(stop) = snapshot.state
            && self.followed != Some(stop)
        {
            self.followed = Some(stop);
            if stop.cpu != self.view.cpu {
                self.change_view(|view| view.cpu = stop.cpu, cx);
            }
        }
        // Linked games: debug the player the keyboard controls.
        let player_cpus: Vec<usize> = (0..snapshot.cpus.len())
            .filter(|cpu| snapshot.cpus[*cpu].player == target.player)
            .collect();
        if !player_cpus.contains(&self.view.cpu)
            && let Some(&cpu) = player_cpus.first()
        {
            self.change_view(|view| view.cpu = cpu, cx);
        }
        // Remember registers while running, to mark what a step changed.
        if snapshot.state == RunState::Running {
            self.previous_registers.clear();
        } else if self.previous_registers.is_empty() {
            self.previous_registers = snapshot.registers.iter().map(|r| r.value).collect();
        }
    }
}

impl gpui::Render for DebuggerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.stale_images.drain(..) {
            window.drop_image(image).ok();
        }
        let root = div()
            .track_focus(&self.focus)
            .key_context("Debugger")
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .font_family("Menlo")
            .text_size(px(11.0))
            .line_height(px(LINE))
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &DebugStep, _, cx| this.step(cx)))
            .on_action(cx.listener(|this, _: &DebugStepOver, _, cx| this.step_over(cx)))
            .on_action(cx.listener(|this, _: &DebugNextFrame, _, cx| this.next_frame(cx)));
        let target = self
            .main
            .upgrade()
            .and_then(|main| main.read(cx).debug_target());
        let Some(target) = target else {
            return root.child(message("Open a game to debug it."));
        };
        self.follow(&target, window, cx);
        let Some(snapshot) = target.snapshot.clone() else {
            return root.child(message("Waiting for the game…"));
        };
        let running = snapshot.state == RunState::Running;
        root.on_action(cx.listener(move |this, _: &TogglePause, _, cx| {
            this.toggle_run(running, cx);
        }))
        .child(self.toolbar(&snapshot, target.player, cx))
        .child(
            div()
                .flex()
                .gap_2()
                .child(self.code_panel(&snapshot, target.system, cx))
                .child(self.register_panel(&snapshot))
                .child(self.memory_panel(&snapshot, target.system, cx)),
        )
        .child(self.lower_panel(&snapshot, target.system, cx))
    }
}

impl DebuggerWindow {
    /// CPU choice, run controls and what the emulation is doing.
    fn toolbar(&self, snapshot: &Snapshot, player: usize, cx: &mut Context<Self>) -> AnyElement {
        let cpus = snapshot
            .cpus
            .iter()
            .enumerate()
            .filter(|(_, cpu)| cpu.player == player);
        let cpu_tabs = cpus.map(|(index, cpu)| {
            tab(("cpu", index), cpu.name, index == snapshot.cpu).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.cursor = None;
                    this.change_view(|view| view.cpu = index, cx);
                },
            ))
        });
        let running = snapshot.state == RunState::Running;
        let state = match snapshot.state {
            RunState::Running => "Running".to_owned(),
            RunState::Paused => "Paused".to_owned(),
            RunState::Stepping => "Running to the next stop…".to_owned(),
            RunState::Stopped(stop) if stop.cpu == snapshot.cpu => {
                debugger::describe_stop(&stop, &snapshot.cpus, snapshot.pc)
            }
            RunState::Stopped(stop) => {
                let name = snapshot.cpus.get(stop.cpu).map_or("CPU", |cpu| cpu.name);
                format!("Stopped by the {name}")
            }
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .children(cpu_tabs)
            .child(
                button(
                    "run",
                    if running {
                        "Pause ⌘P"
                    } else {
                        "Continue ⌘P"
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_run(running, cx))),
            )
            .child(button("step", "Step ⌘⇧I").on_click(cx.listener(|this, _, _, cx| this.step(cx))))
            .child(
                button("over", "Step over ⌘⇧O")
                    .on_click(cx.listener(|this, _, _, cx| this.step_over(cx))),
            )
            .child(
                button("frame", "Next frame ⌘⇧N")
                    .on_click(cx.listener(|this, _, _, cx| this.next_frame(cx))),
            )
            .child(
                div()
                    .text_color(rgb(if running { DIM } else { CHANGED }))
                    .child(state),
            )
            .into_any_element()
    }

    /// The disassembly, with a gutter to click for breakpoints.
    fn code_panel(
        &self,
        snapshot: &Snapshot,
        system: System,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let following = self.view.code.is_none();
        // ARM's status line names the state; Thumb encodings are halfwords.
        let thumb = snapshot.status.contains("Thumb");
        let lines = snapshot.code.iter().enumerate().map(|(index, line)| {
            let address = line.address;
            let marked = self.breakpoints.contains(&Breakpoint {
                cpu: snapshot.cpu,
                address,
            });
            let bytes = encoding(&line.bytes, system, thumb);
            let gutter = div()
                .id(("gutter", index))
                .flex_none()
                .w(px(14.0))
                .cursor_pointer()
                .text_color(rgb(BREAKPOINT))
                .child(if marked { "●" } else { " " })
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_breakpoint(address, cx)));
            let row = div().flex().child(gutter).child(format!(
                "{} {:<9} {}",
                memory_viewer::format_address(address),
                bytes,
                line.text
            ));
            if address == snapshot.pc {
                row.bg(rgb(ACCENT))
            } else {
                row
            }
        });
        panel("Disassembly")
            .w(px(380.0))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(self.entry_or_button(Field::Code, "Go to…", cx))
                    .when(!following, |row| {
                        row.child(button("follow", "Follow PC").on_click(cx.listener(
                            |this, _, _, cx| this.change_view(|view| view.code = None, cx),
                        )))
                    }),
            )
            .child(div().whitespace_nowrap().overflow_hidden().children(lines))
            .into_any_element()
    }

    /// Registers in two columns, changed ones highlighted, then the status.
    fn register_panel(&self, snapshot: &Snapshot) -> AnyElement {
        let half = snapshot.registers.len().div_ceil(2);
        let column = |registers: &[emu_core::debug::Register], offset: usize| {
            div()
                .flex()
                .flex_col()
                .children(registers.iter().enumerate().map(|(index, register)| {
                    let changed = self
                        .previous_registers
                        .get(offset + index)
                        .is_some_and(|previous| *previous != register.value);
                    let digits = usize::from(register.bits / 4);
                    div()
                        .text_color(rgb(if changed { CHANGED } else { TEXT }))
                        .child(format!("{:<4} {:0digits$X}", register.name, register.value))
                }))
        };
        let (left, right) = snapshot
            .registers
            .split_at(half.min(snapshot.registers.len()));
        panel("Registers")
            .w(px(220.0))
            .child(
                div()
                    .flex()
                    .gap_4()
                    .child(column(left, 0))
                    .child(column(right, half)),
            )
            .child(div().text_color(rgb(DIM)).child(snapshot.status.clone()))
            .into_any_element()
    }

    /// Hex rows: click a byte, then type hex digits to change it.
    fn memory_panel(
        &self,
        snapshot: &Snapshot,
        system: System,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let regions = memory_viewer::regions(system)
            .iter()
            .enumerate()
            .map(|(index, region)| {
                let start = region.start;
                tab(("region", index), region.name, false).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.memory
                            .go_to(start, memory_viewer::last_address(system));
                        let address = this.memory.address;
                        this.change_view(|view| view.memory = address, cx);
                    },
                ))
            });
        let rows = snapshot
            .memory
            .chunks(BYTES_PER_ROW as usize)
            .enumerate()
            .map(|(row, bytes)| {
                let row_address = snapshot
                    .memory_start
                    .wrapping_add(row as u32 * BYTES_PER_ROW);
                let cells = bytes.iter().enumerate().map(|(column, byte)| {
                    let address = row_address.wrapping_add(column as u32);
                    let selected = self.cursor.filter(|cursor| cursor.address == address);
                    let text = match selected.and_then(|cursor| cursor.high) {
                        Some(high) => format!("{high:X}_"),
                        None => memory_viewer::format_byte(*byte),
                    };
                    let cell = div()
                        .id(("byte", row * BYTES_PER_ROW as usize + column))
                        .w(px(20.0))
                        .cursor_pointer()
                        .child(text)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.entry = None;
                                this.cursor = Some(Cursor::at(address));
                                cx.notify();
                            }),
                        );
                    if selected.is_some() {
                        cell.bg(rgb(ACCENT))
                    } else {
                        cell
                    }
                });
                div()
                    .flex()
                    .child(
                        div()
                            .w(px(70.0))
                            .text_color(rgb(DIM))
                            .child(memory_viewer::format_address(row_address)),
                    )
                    .children(cells)
                    .child(
                        div()
                            .pl_2()
                            .text_color(rgb(DIM))
                            .child(memory_viewer::ascii(bytes)),
                    )
            });
        panel("Memory — click a byte and type hex to change it")
            .flex_1()
            .child(div().flex().flex_wrap().gap_1().children(regions))
            .child(self.entry_or_button(Field::Memory, "Go to…", cx))
            .child(
                div()
                    .id("memory-rows")
                    .whitespace_nowrap()
                    .children(rows)
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        this.scroll_memory(event, cx);
                    })),
            )
            .into_any_element()
    }

    /// The tabs below, and the one shown.
    fn lower_panel(
        &mut self,
        snapshot: &Snapshot,
        system: System,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tabs = [
            (Panel::Breakpoints, "Breakpoints"),
            (Panel::Graphics, "Graphics"),
            (Panel::Io, "I/O registers"),
            (Panel::Events, "Scheduler"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (panel, name))| {
            tab(("panel", index), name, self.view.panel == panel).on_click(
                cx.listener(move |this, _, _, cx| this.change_view(|view| view.panel = panel, cx)),
            )
        })
        .collect::<Vec<_>>();
        let content = match self.view.panel {
            Panel::Breakpoints => self.stops_panel(snapshot, cx),
            Panel::Graphics => self.graphics_panel(snapshot, system, cx),
            Panel::Io => lines(snapshot.io.iter().map(|register| {
                let digits = usize::from(register.bits / 4);
                format!(
                    "{}  {:<16} {:0digits$X}",
                    memory_viewer::format_address(register.address),
                    register.name,
                    register.value
                )
            }))
            .into_any_element(),
            Panel::Events if snapshot.events.is_empty() => {
                message("No scheduler: this core ticks every component each cycle.")
                    .into_any_element()
            }
            Panel::Events => lines(
                snapshot
                    .events
                    .iter()
                    .map(|event| format!("in {:>9} cycles  {}", event.cycles, event.name)),
            )
            .into_any_element(),
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_1()
            .min_h(px(0.0))
            .child(div().flex().gap_1().children(tabs))
            .child(
                div()
                    .id("lower")
                    .flex_1()
                    .p_2()
                    .bg(rgb(PANEL))
                    .rounded_md()
                    .overflow_y_scroll()
                    .child(content),
            )
            .into_any_element()
    }

    /// The breakpoint and watchpoint lists, with buttons to add and remove.
    fn stops_panel(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let cpu_name = |cpu: usize| snapshot.cpus.get(cpu).map_or("CPU", |info| info.name);
        let breakpoints = self
            .breakpoints
            .iter()
            .enumerate()
            .map(|(index, breakpoint)| {
                div()
                    .flex()
                    .gap_2()
                    .child(button(("remove-break", index), "✕").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.breakpoints.remove(index);
                            this.send_stops(cx);
                            cx.notify();
                        },
                    )))
                    .child(format!(
                        "{} {}",
                        cpu_name(breakpoint.cpu),
                        memory_viewer::format_address(breakpoint.address)
                    ))
            });
        let watchpoints = self.watchpoints.iter().enumerate().map(|(index, watch)| {
            div()
                .flex()
                .gap_2()
                .child(button(("remove-watch", index), "✕").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.watchpoints.remove(index);
                        this.send_stops(cx);
                        cx.notify();
                    },
                )))
                .child(format!(
                    "{} {}-{} {}",
                    cpu_name(watch.cpu),
                    memory_viewer::format_address(watch.start),
                    memory_viewer::format_address(watch.end),
                    kind_name(watch.kind)
                ))
        });
        let kinds = [WatchKind::Read, WatchKind::Write, WatchKind::Access]
            .into_iter()
            .enumerate()
            .map(|(index, kind)| {
                tab(("kind", index), kind_name(kind), self.watch_kind == kind).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.watch_kind = kind;
                        cx.notify();
                    }),
                )
            });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child("Breakpoints (or click the gutter beside an instruction)")
                    .child(self.entry_or_button(Field::Breakpoint, "Add…", cx)),
            )
            .children(breakpoints)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .pt_2()
                    .child("Watchpoints: stop after")
                    .children(kinds)
                    .child(self.entry_or_button(Field::Watchpoint, "Add…", cx)),
            )
            .children(watchpoints)
            .child(div().pt_2().text_color(rgb(DIM)).child(
                "Watchpoints see the CPU's own loads and stores, not DMA or the HLE BIOS's.",
            ))
            .into_any_element()
    }

    /// The graphics viewer's choices, picture and notes.
    fn graphics_panel(
        &mut self,
        snapshot: &Snapshot,
        system: System,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let request = self.view.graphics;
        let set = |change: fn(&mut GraphicsRequest, u32), value: u32| {
            move |this: &mut Self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                this.change_view(|view| change(&mut view.graphics, value), cx);
            }
        };
        let views = [
            (GraphicsView::Tiles, "Tiles"),
            (GraphicsView::Map, "Map"),
            (GraphicsView::Sprites, "Sprites"),
            (GraphicsView::Palettes, "Palettes"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (view, name))| {
            tab(("view", index), name, request.view == view).on_click(
                cx.listener(move |this, _, _, cx| this.change_view(|v| v.graphics.view = view, cx)),
            )
        });
        let mut controls = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .items_center()
            .children(views);
        if system == System::Nds {
            controls = controls.child(div().pl_2().child("Engine")).children(
                ["A", "B"].into_iter().enumerate().map(|(index, name)| {
                    tab(
                        ("engine", index),
                        name,
                        usize::from(request.engine) == index,
                    )
                    .on_click(cx.listener(set(|r, v| r.engine = v as u8, index as u32)))
                }),
            );
        }
        match request.view {
            GraphicsView::Map => {
                let layers: &[&str] = if system == System::Gb {
                    &["9800", "9C00"]
                } else {
                    &["BG0", "BG1", "BG2", "BG3"]
                };
                controls = controls.child(div().pl_2().child("Layer")).children(
                    layers.iter().enumerate().map(|(index, name)| {
                        tab(("layer", index), name, usize::from(request.layer) == index)
                            .on_click(cx.listener(set(|r, v| r.layer = v as u8, index as u32)))
                    }),
                );
            }
            GraphicsView::Tiles => {
                if system != System::Gb {
                    let bases =
                        tile_bases(system)
                            .iter()
                            .enumerate()
                            .map(|(index, (name, base))| {
                                tab(("base", index), name, request.base == *base)
                                    .on_click(cx.listener(set(|r, v| r.base = v, *base)))
                            });
                    controls = controls
                        .child(div().pl_2().child(format!("From {:08X}", request.base)))
                        .children(bases)
                        .child(button("base-down", "−32K").on_click(
                            cx.listener(set(|r, _| r.base = r.base.wrapping_sub(0x8000), 0)),
                        ))
                        .child(button("base-up", "+32K").on_click(
                            cx.listener(set(|r, _| r.base = r.base.wrapping_add(0x8000), 0)),
                        ))
                        .child(self.entry_or_button(Field::TileBase, "From…", cx))
                        .child(
                            tab(
                                "bpp",
                                if request.eight_bit { "8bpp" } else { "4bpp" },
                                false,
                            )
                            .on_click(cx.listener(set(|r, _| r.eight_bit = !r.eight_bit, 0))),
                        );
                }
                controls =
                    controls
                        .child(div().pl_2().child(format!("Palette {}", request.palette)))
                        .child(button("palette-down", "−").on_click(
                            cx.listener(set(|r, _| r.palette = r.palette.saturating_sub(1), 0)),
                        ))
                        .child(button("palette-up", "+").on_click(
                            cx.listener(set(|r, _| r.palette = (r.palette + 1).min(31), 0)),
                        ));
            }
            GraphicsView::Sprites | GraphicsView::Palettes => {}
        }
        let Some(graphics) = &snapshot.graphics else {
            return div()
                .flex()
                .flex_col()
                .gap_2()
                .child(controls)
                .child(message("This core cannot draw that."))
                .into_any_element();
        };
        // Build the image only when the picture changed.
        if self
            .picture
            .as_ref()
            .is_none_or(|(frame, _)| *frame != graphics.image)
        {
            let size = graphics.image.size();
            let factor = if size.width <= 256 { 4 } else { 2 };
            let image = video::render_image(&graphics.image, factor);
            if let Some((_, old)) = self.picture.replace((graphics.image.clone(), image)) {
                self.stale_images.push(old);
            }
        }
        let size = graphics.image.size();
        let zoom = if size.width <= 256 { 2.0 } else { 1.0 };
        let picture = self.picture.as_ref().map(|(_, image)| {
            img(Arc::clone(image))
                .flex_none()
                .w(px(f32::from(size.width) * zoom))
                .h(px(f32::from(size.height) * zoom))
                .object_fit(ObjectFit::Fill)
        });
        // The notes go below: sprite lists are wider than any window.
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(controls)
            .children(picture)
            .child(lines(graphics.notes.iter().cloned()))
            .into_any_element()
    }

    /// A field being typed into, or a button that starts typing it.
    fn entry_or_button(&self, field: Field, label: &'static str, cx: &Context<Self>) -> AnyElement {
        match &self.entry {
            Some(entry) if entry.field == field => div()
                .px_1()
                .bg(rgb(ACCENT))
                .rounded_sm()
                .child(format!("{}: {}▏ (Return, Esc)", field.prompt(), entry.text))
                .into_any_element(),
            _ => button(SharedString::from(format!("{field:?}")), label)
                .on_click(cx.listener(move |this, _, _, cx| this.start_entry(field, cx)))
                .into_any_element(),
        }
    }
}

/// An instruction's encoding as its manuals print it: the Game Boy's
/// bytes in order; an ARM word or Thumb halfwords as numbers.
fn encoding(bytes: &[u8], system: System, thumb: bool) -> String {
    if system == System::Gb {
        let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
        return hex.join(" ");
    }
    let unit = if thumb { 2 } else { 4 };
    let words: Vec<String> = bytes
        .chunks(unit)
        .map(|chunk| {
            let value = chunk
                .iter()
                .rev()
                .fold(0u32, |value, byte| value << 8 | u32::from(*byte));
            format!("{value:0width$X}", width = chunk.len() * 2)
        })
        .collect();
    words.join(" ")
}

/// Places to draw tiles from, by name and address.
fn tile_bases(system: System) -> &'static [(&'static str, u32)] {
    match system {
        System::Gb | System::RpgMaker => &[],
        System::Gba => &[("BG", 0x0600_0000), ("OBJ", 0x0601_0000)],
        System::Nds => &[
            ("A BG", 0x0600_0000),
            ("B BG", 0x0620_0000),
            ("A OBJ", 0x0640_0000),
            ("B OBJ", 0x0660_0000),
            ("LCDC", 0x0680_0000),
        ],
    }
}

fn kind_name(kind: WatchKind) -> &'static str {
    match kind {
        WatchKind::Read => "read",
        WatchKind::Write => "write",
        WatchKind::Access => "access",
    }
}

/// A titled box.
fn panel(title: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .bg(rgb(PANEL))
        .rounded_md()
        .child(div().text_color(rgb(DIM)).child(title))
}

/// A clickable label.
fn button(id: impl Into<gpui::ElementId>, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_2()
        .rounded_sm()
        .cursor_pointer()
        .bg(rgb(0x0037_3737))
        .hover(|style| style.bg(rgb(0x0045_4545)))
        .child(label)
}

/// A button that shows whether it is the one chosen.
fn tab(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    chosen: bool,
) -> gpui::Stateful<gpui::Div> {
    let tab = button(id, label);
    if chosen { tab.bg(rgb(ACCENT)) } else { tab }
}

/// Lines of text, one per row.
fn lines(lines: impl Iterator<Item = String>) -> gpui::Div {
    div().flex().flex_col().whitespace_nowrap().children(lines)
}

fn message(text: &'static str) -> gpui::Div {
    div().p_4().text_color(rgb(DIM)).child(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings_read_as_the_manuals_print_them() {
        assert_eq!(encoding(&[0x3E, 0x42], System::Gb, false), "3E 42");
        let mov = [0x09, 0x70, 0xA0, 0xE1];
        assert_eq!(encoding(&mov, System::Gba, false), "E1A07009");
        let bl_pair = [0x00, 0xF0, 0x00, 0xF8];
        assert_eq!(encoding(&bl_pair, System::Nds, true), "F000 F800");
    }
}
