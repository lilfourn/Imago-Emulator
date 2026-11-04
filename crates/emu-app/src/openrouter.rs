//! The player's OpenRouter connection, for Guide
//! ([PRD](../../../docs/PRD-game-chat.md)): whether a key is saved,
//! connecting one through the browser as Imago does, and what OpenRouter
//! says the key has spent. One for the whole app, kept as a global, so
//! connecting carries on when the settings window closes.
//!
//! Nothing reaches the network until the player connects; after that,
//! only showing the settings window asks OpenRouter about the key.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use emu_guide::api::Endpoints;
use emu_guide::key::{KeyStore, Source};
use emu_guide::login::{self, LoginError, Progress};
use gpui::{App, AppContext, Context, Entity, Global, Task};

/// Where the player manages their keys on OpenRouter: limits, renaming,
/// revoking.
pub const KEYS_PAGE: &str = "https://openrouter.ai/settings/keys";

/// How the connection stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// No key: Guide can't ask anything yet.
    NotConnected,
    /// Waiting on the browser; `returned` once it came back with a code,
    /// which is being exchanged for a key.
    Connecting {
        /// Whether the browser is back.
        returned: bool,
    },
    /// A key to use.
    Connected {
        /// The key's last characters, to tell keys apart.
        tail: String,
        /// Whether it is `OPENROUTER_API_KEY`'s rather than the saved one.
        from_environment: bool,
        /// What OpenRouter says of it.
        usage: Usage,
    },
    /// The last try failed, for this reason.
    Failed(String),
}

/// What OpenRouter says of a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Usage {
    /// Not asked yet, or no answer.
    Unknown,
    /// What it has spent, as "$0.42 used" or "Free tier".
    Known(String),
    /// OpenRouter no longer accepts it: deleted or disabled on its site.
    Refused,
}

/// The connection, as an entity the settings window watches.
pub struct OpenRouter {
    store: KeyStore,
    status: Status,
    /// Set to stop the connecting thread's wait, while one runs.
    cancel: Option<Arc<AtomicBool>>,
    /// Hands the connecting thread's progress to this entity.
    flow: Option<Task<()>>,
    /// Asks OpenRouter about the key.
    checking: Option<Task<()>>,
}

/// The app's one [`OpenRouter`].
struct Shared(Entity<OpenRouter>);

impl Global for Shared {}

/// What the connecting thread says.
enum Event {
    Progress(Progress),
    Done(Result<(), LoginError>),
}

impl OpenRouter {
    /// The app's connection, made on first use.
    pub fn global(cx: &mut App) -> Entity<Self> {
        if let Some(shared) = cx.try_global::<Shared>() {
            return shared.0.clone();
        }
        let store = KeyStore::new(
            dirs::data_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("ds-emulator")
                .join("OpenRouter"),
        );
        let entity = cx.new(|_| Self::new(store));
        cx.set_global(Shared(entity.clone()));
        entity
    }

    /// A connection keeping its key in `store`, as it stands now.
    fn new(store: KeyStore) -> Self {
        let mut this = Self {
            store,
            status: Status::NotConnected,
            cancel: None,
            flow: None,
            checking: None,
        };
        this.reload();
        this
    }

    /// How the connection stands.
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Connects an account: opens OpenRouter's page in the browser and
    /// waits, on a thread of its own, for the player to come back.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Connecting { .. }) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (events, received) = async_channel::unbounded();
        let store = self.store.clone();
        let stop = cancel.clone();
        let spawned = thread::Builder::new()
            .name("openrouter-connect".to_owned())
            .spawn(move || {
                let label = login::label(&login::device_name());
                let outcome = login::connect(
                    &Endpoints::openrouter(),
                    &store,
                    &label,
                    login::TIMEOUT,
                    &stop,
                    |progress| {
                        events.send_blocking(Event::Progress(progress)).ok();
                    },
                );
                events.send_blocking(Event::Done(outcome)).ok();
            });
        if let Err(error) = spawned {
            self.status = Status::Failed(format!("Couldn't start connecting: {error}"));
            cx.notify();
            return;
        }
        self.cancel = Some(cancel);
        self.status = Status::Connecting { returned: false };
        self.flow = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = received.recv().await {
                if this
                    .update(cx, |this, cx| this.on_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// Stops waiting for the browser. A key already on its way is still
    /// saved.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
            self.reload();
            cx.notify();
        }
    }

    /// Forgets the saved key. It still works on OpenRouter until the
    /// player revokes it there.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.store.forget() {
            self.status = Status::Failed(format!("The key couldn't be removed: {error}"));
        } else {
            self.reload();
        }
        cx.notify();
    }

    /// Asks OpenRouter what the key has spent, and whether it still
    /// accepts it, off the UI thread.
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.status, Status::Connected { .. }) {
            return;
        }
        let store = self.store.clone();
        let asking = cx.background_executor().spawn(async move {
            let key = store.load()?;
            let info = Endpoints::openrouter().key_info(key.secret());
            Some((key.tail().to_owned(), info))
        });
        self.checking = Some(cx.spawn(async move |this, cx| {
            let Some((tail, info)) = asking.await else {
                return;
            };
            this.update(cx, |this, cx| {
                // The key may have changed while OpenRouter answered.
                if let Status::Connected {
                    tail: shown, usage, ..
                } = &mut this.status
                    && *shown == tail
                {
                    *usage = match info {
                        Ok(info) => info.summary().map_or(Usage::Unknown, Usage::Known),
                        Err(error) if error.is_refused_key() => Usage::Refused,
                        Err(_) => Usage::Unknown,
                    };
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Takes in what the connecting thread said.
    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        // Once cancelled, only a key that arrived anyway matters.
        let waiting = self.cancel.is_some();
        match event {
            Event::Progress(Progress::Open(url)) if waiting => cx.open_url(&url),
            Event::Progress(Progress::Returned) if waiting => {
                self.status = Status::Connecting { returned: true };
            }
            Event::Done(Ok(())) => {
                self.cancel = None;
                self.reload();
                self.check(cx);
            }
            Event::Done(Err(error)) if waiting => {
                self.cancel = None;
                self.status = Status::Failed(error.to_string());
            }
            _ => return,
        }
        cx.notify();
    }

    /// Reads the key anew.
    fn reload(&mut self) {
        self.status = match self.store.load() {
            Some(key) => Status::Connected {
                tail: key.tail().to_owned(),
                from_environment: key.source() == Source::Environment,
                usage: Usage::Unknown,
            },
            None => Status::NotConnected,
        };
    }
}
