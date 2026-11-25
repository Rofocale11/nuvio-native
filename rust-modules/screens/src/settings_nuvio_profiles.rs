//! Fork Rofocale11: Settings > Profiles — manage the Nuvio account's profile slots 1–6.
//!
//! Ported from the `settings_addons.rs` pattern: a `FormTable` page over the shared family
//! plumbing, with the pure builder separated from the registered control so a fit test can drive
//! the real form without touching the shim. Every account write sends the COMPLETE roster: the
//! public API fully replaces slots 1..6 and deletes anything omitted.
//!
//! The account's `pin_enabled` is read-only and advisory. A tile is locked on this TV only when
//! a local verifier exists, created here and checked by the shim before a profile switch.

use std::borrow::Cow;

use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind,
    InputOwner, InstanceId, Key, LogicalState, Machine, MachineId, ScreenEvent, TextEdit,
};
use plx_machine::present::Provenance;
use plx_plex::plex::nuvio_profiles as control;
use plx_ui::form::{Form, FormId, FormSection, FormTable, RowKey, RowKind};
use plx_ui::frame::Budget;
use plx_ui::route_screen::RouteLayout;
use plx_ui::screen::{DrawFrame, Enter, FocusSource, FocusTarget, HitSource, RenderStrategy, Screen};
use plx_ui::table::Row;
use plx_ui::table_screen::{Header, TableScreen};
use plx_ui::text_buffer::TextBuffer;
use plx_ui::widgets::Spinner;
use plx_ui::{theme, Env, Rect, View};

use super::super::family::{form_activate, form_focus, InnerHost, SettingsPage};
use super::super::registry::AppFx;

#[derive(Clone, PartialEq, Eq, Debug)]
enum ProfId {
    Slot(u32),
    Add,
    Rename,
    Avatar,
    Primary,
    PinSet,
    PinClear,
    ForgotPin,
    Delete,
}

impl FormId for ProfId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            ProfId::Slot(i) => *i,
            ProfId::Add => 100,
            ProfId::Rename => 101,
            ProfId::Avatar => 102,
            ProfId::Primary => 103,
            ProfId::PinSet => 104,
            ProfId::PinClear => 105,
            ProfId::ForgotPin => 107,
            ProfId::Delete => 106,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum ProfAction {
    Select(u32),
    Add,
    Rename,
    Avatar,
    Primary,
    PinSet,
    PinClear,
    ForgotPin,
    Delete,
}

/// The real form from supplied values, so the text-fit suite drives the same builder the page
/// draws. A profile's own name and avatar name are user text; every other label is app-owned.
fn profiles_form(
    profiles: &[control::ProfileView],
    target: u32,
    draft: &str,
    editing_name: bool,
    pin_draft: &str,
    avatar_label: &str,
    delete_armed: bool,
) -> Form<ProfId, ProfAction, SettingsPage> {
    // Rows that raise the television keyboard come FIRST. The LG keyboard paints over the lower
    // half of the screen, so a PIN or name field left at the bottom is hidden the moment it is
    // actually needed — the field appeared to do nothing while the digits went somewhere unseen.
    let locked = profiles.iter().find(|p| p.profile_index == target).is_some_and(|p| p.tv_locked);
    let mut keys = FormSection::new("");
    keys = keys.item(ProfId::Rename, RowKind::Button, ProfAction::Rename,
        Row::new(plx_os::i18n::msg::nuvio_profiles_rename()).value(if editing_name { draft } else { "" }));
    keys = keys.item(ProfId::PinSet, RowKind::Button, ProfAction::PinSet,
        Row::new(plx_os::i18n::msg::nuvio_profiles_pin_set()).value(pin_draft));
    keys = keys.item(ProfId::PinClear, RowKind::Button, ProfAction::PinClear,
        Row::new(plx_os::i18n::msg::nuvio_profiles_pin_clear())).disabled(!locked);

    let mut list = FormSection::new("");
    for p in profiles {
        let mut suffix = String::new();
        if p.profile_index == target { suffix.push('•'); }
        if p.tv_locked {
            if !suffix.is_empty() { suffix.push(' '); }
            suffix.push_str("PIN");
        }
        list = list.item(
            ProfId::Slot(p.profile_index),
            RowKind::Button,
            ProfAction::Select(p.profile_index),
            Row::new(&p.name).server_label().value(suffix),
        );
    }
    let mut actions = FormSection::new("");
    actions = actions.item(ProfId::Add, RowKind::Button, ProfAction::Add,
        Row::new(plx_os::i18n::msg::nuvio_profiles_add()).disabled(profiles.len() >= 6));
    actions = actions.item(ProfId::Avatar, RowKind::Button, ProfAction::Avatar,
        Row::new(plx_os::i18n::msg::nuvio_profiles_avatar()).server_value().value(avatar_label));
    actions = actions.item(ProfId::Primary, RowKind::Toggle, ProfAction::Primary,
        Row::new(plx_os::i18n::msg::nuvio_profiles_primary())
            .toggle(profiles.iter().find(|p| p.profile_index == target).is_some_and(|p| p.uses_primary_addons)));
    actions = actions.item(ProfId::ForgotPin, RowKind::Button, ProfAction::ForgotPin,
        Row::new(plx_os::i18n::msg::nuvio_profiles_forgot_pin())).disabled(!locked);
    actions = actions.item(ProfId::Delete, RowKind::Button, ProfAction::Delete,
        Row::new(plx_os::i18n::msg::settings_delete())
            .value(if delete_armed { plx_os::i18n::msg::nuvio_profiles_delete_again().to_string() } else { String::new() })
            .destructive(true));
    // Slot 1 is the account's main profile: the API refuses to delete it, so arming a delete there
    // could never complete and only left the row stuck on its confirmation. This is the FORM's
    // disabled state (inert to OK), not the Row's look-only flag.
    actions = actions.disabled(target == 1);
    Form::new().section(keys).section(list).section(actions)
}

struct ProfilesState {
    sel: RowKey,
    profiles: Vec<control::ProfileView>,
    avatars: Vec<control::AvatarView>,
    avatar_at: usize,
    target: u32,
    name: TextBuffer,
    editing_name: bool,
    /// True while the television keyboard is up for this page; text edits arrive only then.
    editing: bool,
    pin: String,
    /// 0 idle, 1 proving the current PIN, 2 new PIN, 3 confirming the new PIN.
    pin_step: u8,
    pin_first: Option<String>,
    current_pin: String,
    clearing: bool,
    delete_armed: bool,
    /// The row whose write is still in flight on a worker. Drawn as a small inline spinner so a
    /// press is acknowledged before the refreshed snapshot lands; cleared on the next generation.
    pending: Option<ProfId>,
}

impl LogicalState for ProfilesState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0);
        w.u32(self.target);
        w.u32(self.profiles.len() as u32);
        w.bool(self.editing_name);
        w.bool(self.editing);
        w.str(self.name.text());
        // A PIN is a secret: the recorded logical state holds only how many digits are typed.
        w.u32(self.pin.len() as u32);
        w.bool(self.delete_armed);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("profiles sel={} target={} n={} edit={} armed={}",
            self.sel.0, self.target, self.profiles.len(), self.editing_name, self.delete_armed));
    }
}

pub struct ProfilesPage {
    entry: EntryId,
    form: FormTable<ProfId, ProfAction, SettingsPage>,
    state: ProfilesState,
    /// The snapshot generation this form was last built from; a worker refresh shows up as a
    /// change on the next tick, with no IO in `step`.
    last_gen: u64,
}

impl ProfilesPage {
    pub fn new(entry: EntryId) -> Self {
        // Read the CACHED snapshot. The application refreshes it once at launch, after the PMS
        // client is installed, and again after each write. A mount is a pure read: no fetch lands
        // under the cursor, so opening the page never re-seats focus on profile 1.
        let profiles = control::profiles();
        let avatars = control::avatars();
        let target = profiles.first().map_or(1, |p| p.profile_index);
        let name = profiles.first().map_or_else(|| "Profile 1".to_string(), |p| p.name.clone());
        let mut page = Self {
            entry,
            form: FormTable::page(super::super::registry::BAND),
            last_gen: control::generation(),
            state: ProfilesState {
                sel: RowKey(0), profiles, avatars, avatar_at: 0, target,
                name: TextBuffer::new(name.clone(), name.len()), editing_name: false, editing: false,
                pin: String::new(), pin_step: 0, pin_first: None,
                current_pin: String::new(), clearing: false, delete_armed: false, pending: None,
            },
        };
        page.rebuild(None, false);
        page
    }

    fn avatar_label(&self) -> String {
        let Some(p) = self.state.profiles.iter().find(|p| p.profile_index == self.state.target) else {
            return String::new();
        };
        if let Some(id) = &p.avatar_id {
            if let Some(a) = self.state.avatars.iter().enumerate().find(|(_, a)| a.id == *id) {
                return a.1.display_name.clone();
            }
        }
        String::new()
    }

    /// `refresh` keeps the pill and the scroll where the viewer left them: a worker snapshot that
    /// lands under the cursor must not move ground. An explicit press still snaps to its own row.
    fn rebuild(&mut self, keep: Option<ProfId>, refresh: bool) {
        let avatar_label = self.avatar_label();
        // Never draw the digits themselves: the pad is a secret even while it is being typed.
        // Before the first digit, say WHICH PIN is wanted — the clear row asks for the current
        // one first, and showing only an empty value made the keyboard look like a dead end.
        let pin_value = if self.state.pin_step > 0 && self.state.pin.is_empty() {
            match self.state.pin_step {
                1 => plx_os::i18n::msg::nuvio_profiles_pin_current().to_string(),
                2 => plx_os::i18n::msg::nuvio_profiles_pin_set().to_string(),
                _ => plx_os::i18n::msg::nuvio_profiles_pin_confirm().to_string(),
            }
        } else {
            "•".repeat(self.state.pin.len())
        };
        let s = &self.state;
        let form = profiles_form(&s.profiles, s.target, s.name.text(), s.editing_name,
            &pin_value, &avatar_label, s.delete_armed);
        if refresh {
            self.form.refresh_with(form, keep.as_ref(), None);
        } else {
            self.form.set(form, keep.as_ref());
        }
        self.state.sel = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(RowKey(0), |k| k);
    }

    fn reload(&mut self, keep: Option<ProfId>) {
        let keep = keep.or_else(|| self.form.selected_id().cloned());
        self.apply_snapshot(control::profiles(), control::avatars(), keep);
    }

    /// The snapshot seam the worker's answer lands on. `keep == None` means "whoever the engine
    /// is on keeps their row", which is what makes a late answer harmless under the cursor.
    fn apply_snapshot(&mut self, profiles: Vec<control::ProfileView>, avatars: Vec<control::AvatarView>, keep: Option<ProfId>) {
        self.state.profiles = profiles;
        self.state.avatars = avatars;
        if !self.state.profiles.iter().any(|p| p.profile_index == self.state.target) {
            self.state.target = self.state.profiles.first().map_or(1, |p| p.profile_index);
        }
        self.state.delete_armed = false;
        self.rebuild(keep, true);
    }

    /// The profile to seat after a delete: the survivor next to the removed row in the roster's
    /// order, else the one before it. Picking the first non-target slot landed on profile 1 from
    /// any other row, which read as the page throwing the viewer back to the top.
    fn neighbour_after_delete(&self, target: u32) -> u32 {
        let order: Vec<u32> = self.state.profiles.iter().map(|p| p.profile_index).collect();
        let Some(at) = order.iter().position(|i| *i == target) else { return 1; };
        order.get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|i| order.get(i)))
            .copied()
            .unwrap_or(1)
    }

    /// The full roster as editable rows. PIN fields are never included: the server owns them.
    fn edits(&self) -> Vec<control::ProfileEditView> {
        self.state.profiles.iter().map(|p| {
            let name = if self.state.editing_name && p.profile_index == self.state.target {
                self.state.name.text().trim().to_string()
            } else { p.name.clone() };
            control::ProfileEditView {
                profile_index: p.profile_index,
                name: if name.is_empty() { p.name.clone() } else { name },
                uses_primary_addons: p.uses_primary_addons,
                avatar_id: p.avatar_id.clone(),
                // The shim resolves catalog avatars to URLs; do not echo one back and hide a
                // future change to the catalog entry.
                avatar_url: None,
                avatar_color_hex: p.avatar_color_hex.clone(),
            }
        }).collect()
    }

    /// A roster write keeps the row that triggered it. Hardcoding Rename here used to yank focus
    /// from Avatar / Share main addons back to the first row on every press.
    fn save(&mut self, fx: &mut Effects<'_, InnerHost>, keep: ProfId) {
        let edits = self.edits();
        control::save(&edits);
        if self.state.editing_name { self.keyboard(false, fx); }
        self.state.editing_name = false;
        self.reload(Some(keep));
    }

    fn reset_pin(&mut self) {
        self.state.pin_step = 0;
        self.state.pin.clear();
        self.state.pin_first = None;
        self.state.current_pin.clear();
        self.state.clearing = false;
    }

    /// Raise or lower the television keyboard. Without `Delivery::Keyboard` the LG sends no
    /// `InputKind::Text` at all: this was why the Set TV PIN row appeared to do nothing.
    fn keyboard(&mut self, up: bool, fx: &mut Effects<'_, InnerHost>) {
        if self.state.editing == up { return; }
        self.state.editing = up;
        fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)), Delivery::Keyboard { up }));
        fx.invalidate(Provenance::Input);
    }

    /// A four-digit draft advances one PIN step. The shim verifies the current PIN on the
    /// worker before it changes or clears a lock; this screen never checks it locally (that
    /// would be another frame-thread request) and never claims a wrong PIN worked.
    fn commit_pin_digits(&mut self, fx: &mut Effects<'_, InnerHost>) {
        if self.state.pin.len() != 4 { return; }
        let terminal = self.state.pin_step == 1 && self.state.clearing || self.state.pin_step == 3;
        let digits = self.state.pin.clone();
        let target = self.state.target;
        match self.state.pin_step {
            1 => {
                self.state.current_pin = digits;
                self.state.pin.clear();
                if self.state.clearing {
                    let current = self.state.current_pin.clone();
                    control::set_pin(target, None, Some(&current));
                    self.reset_pin();
                } else {
                    self.state.pin_step = 2;
                }
            },
            2 => {
                self.state.pin_first = Some(digits);
                self.state.pin.clear();
                self.state.pin_step = 3;
            },
            3 => {
                if self.state.pin_first.as_deref() == Some(digits.as_str()) {
                    let current = if self.state.current_pin.is_empty() { None }
                        else { Some(self.state.current_pin.clone()) };
                    control::set_pin(target, Some(&digits), current.as_deref());
                }
                self.reset_pin();
                self.reload(Some(ProfId::PinSet));
            },
            _ => {},
        }
        if terminal { self.keyboard(false, fx); }
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(RouteLayout::screen(), Some(plx_os::i18n::msg::settings_title()),
                plx_os::i18n::msg::settings_profiles_title(), plx_os::i18n::msg::nuvio_profiles_detail()),
            &self.form.table, GroupId(0), self.entry,
        ).keyed(&self.form)
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        let Some(action) = form_activate(&self.form, key, fx) else { return };
        match action {
            ProfAction::Select(index) => {
                self.state.target = index;
                self.state.editing_name = false;
                self.reset_pin();
                self.state.delete_armed = false;
                if let Some(p) = self.state.profiles.iter().find(|p| p.profile_index == index) {
                    self.state.name = TextBuffer::new(p.name.clone(), p.name.len());
                }
                self.rebuild(Some(ProfId::Slot(index)), false);
            },
            ProfAction::Add => {
                let free = (1..=6).find(|n| !self.state.profiles.iter().any(|p| p.profile_index == *n));
                if let Some(index) = free {
                    let name = format!("Profile {index}");
                    let mut edits = self.edits();
                    edits.push(control::ProfileEditView {
                        profile_index: index, name: name.clone(), uses_primary_addons: false,
                        avatar_id: None, avatar_url: None, avatar_color_hex: None,
                    });
                    if control::save(&edits) {
                        self.state.target = index;
                        self.state.name = TextBuffer::new(name.clone(), 6);
                        // Acknowledge the press on the NEW profile's name row. Seat it locally so
                        // the row exists for the spinner; the worker's answer replaces it shortly.
                        self.state.profiles.push(control::ProfileView {
                            profile_index: index, name, ..Default::default()
                        });
                        self.state.pending = Some(ProfId::Slot(index));
                        self.rebuild(Some(ProfId::Slot(index)), false);
                    } else {
                        self.reload(None);
                    }
                }
            },
            ProfAction::Rename => {
                if self.state.editing_name {
                    self.save(fx, ProfId::Rename);
                } else {
                    self.state.editing_name = true;
                    self.keyboard(true, fx);
                }
                self.rebuild(Some(ProfId::Rename), false);
            },
            ProfAction::Avatar => {
                if !self.state.avatars.is_empty() {
                    self.state.avatar_at = (self.state.avatar_at + 1) % self.state.avatars.len();
                    let id = self.state.avatars[self.state.avatar_at].id.clone();
                    for p in self.state.profiles.iter_mut().filter(|p| p.profile_index == self.state.target) {
                        p.avatar_id = Some(id.clone());
                        p.avatar_url = String::new();
                    }
                    self.save(fx, ProfId::Avatar);
                }
            },
            ProfAction::Primary => {
                for p in self.state.profiles.iter_mut().filter(|p| p.profile_index == self.state.target) {
                    p.uses_primary_addons = !p.uses_primary_addons;
                }
                self.save(fx, ProfId::Primary);
            },
            ProfAction::PinSet => {
                if self.state.pin_step == 0 {
                    self.state.clearing = false;
                    self.state.pin_step = if self.state.profiles.iter()
                        .any(|p| p.profile_index == self.state.target && p.tv_locked) { 1 } else { 2 };
                    self.keyboard(true, fx);
                }
                self.rebuild(Some(ProfId::PinSet), false);
            },
            ProfAction::PinClear => {
                if self.state.pin_step == 0 {
                    self.state.clearing = true;
                    let locked = self.state.profiles.iter()
                        .any(|p| p.profile_index == self.state.target && p.tv_locked);
                    if locked {
                        self.state.pin_step = 1;
                        self.keyboard(true, fx);
                    } else {
                        control::set_pin(self.state.target, None, None);
                        self.reset_pin();
                        self.reload(Some(ProfId::PinClear));
                        return;
                    }
                }
                self.rebuild(Some(ProfId::PinClear), false);
            },
            ProfAction::ForgotPin => {
                // Arm on a worker, then open the QR sign-in the app already has. The shim clears
                // this slot's verifier only when that QR is approved as the same account.
                if control::forgot_pin(self.state.target) {
                    self.state.pending = Some(ProfId::ForgotPin);
                }
                fx.push(Fx::App(AppFx::Session(plx_session::auth::SessionCmd::StartLogin)));
                fx.invalidate(Provenance::Input);
            },
            ProfAction::Delete => {
                if self.state.delete_armed {
                    if control::delete(self.state.target) {
                        self.state.pending = Some(ProfId::Delete);
                        let next = self.neighbour_after_delete(self.state.target);
                        self.state.target = next;
                        if let Some(p) = self.state.profiles.iter().find(|p| p.profile_index == next) {
                            self.state.name = TextBuffer::new(p.name.clone(), p.name.len());
                        }
                    }
                    self.reload(Some(ProfId::Slot(self.state.target)));
                } else {
                    self.state.delete_armed = true;
                    self.rebuild(Some(ProfId::Delete), false);
                }
            },
        }
        fx.invalidate(Provenance::Input);
    }

    fn edit(&mut self, edit: TextEdit, fx: &mut Effects<'_, InnerHost>) {
        let pin_key = if self.state.clearing { ProfId::PinClear } else { ProfId::PinSet };
        if self.state.editing_name {
            self.state.name.edit(&edit);
        } else if self.state.pin_step > 0 {
            match edit {
                TextEdit::Commit(text) => {
                    for b in text.bytes().filter(|b| b.is_ascii_digit()) {
                        if self.state.pin.len() < 4 { self.state.pin.push(b as char); }
                    }
                },
                TextEdit::Backspace => { self.state.pin.pop(); },
                TextEdit::Clear => { self.state.pin.clear(); },
                TextEdit::Left | TextEdit::Right => {},
            }
            if self.state.pin.len() == 4 { self.commit_pin_digits(fx); }
        }
        self.rebuild(Some(if self.state.editing_name { ProfId::Rename } else { pin_key }), false);
        fx.invalidate(Provenance::Input);
    }

    /// Text/keys only while this page owns the television keyboard. The remote's number keys
    /// arrive as `Text` commits once `Delivery::Keyboard{up:true}` has been emitted.
    fn typing(&mut self, kind: &InputKind<u32>, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match kind {
            InputKind::SystemKeyboard(up) => {
                if !up && self.state.editing {
                    self.state.editing = false;
                    self.state.pin_step = 0;
                    self.state.pin.clear();
                    self.state.editing_name = false;
                    self.rebuild(None, false);
                    fx.invalidate(Provenance::Input);
                }
                Handled::Yes
            },
            InputKind::Text(edit) if self.state.editing || matches!(cx.owner, InputOwner::System(_)) => {
                self.edit(edit.clone(), fx);
                Handled::Yes
            },
            InputKind::Key { key, sym, edge, .. } if self.state.editing => {
                if *edge == Edge::Up { return Handled::Yes; }
                let edit = match (*key, *sym) {
                    (Key::Left, _) => Some(TextEdit::Left),
                    (Key::Right, _) => Some(TextEdit::Right),
                    (_, plx_ui::consts::SDLK_BACKSPACE) => Some(TextEdit::Backspace),
                    (_, plx_ui::consts::SDLK_CLEAR) => Some(TextEdit::Clear),
                    _ => None,
                };
                match (edit, *key) {
                    (Some(edit), _) => self.edit(edit, fx),
                    (None, Key::Back) => {
                        self.keyboard(false, fx);
                        self.state.pin_step = 0;
                        self.state.pin.clear();
                        self.state.editing_name = false;
                        self.rebuild(None, false);
                    },
                    (None, Key::Ok) => {
                        if self.state.editing_name { self.save(fx, ProfId::Rename); }
                        else if self.state.pin.len() == 4 { self.commit_pin_digits(fx); }
                    },
                    _ => {},
                }
                Handled::Yes
            },
            _ => Handled::No,
        }
    }
}

impl Machine<InnerHost> for ProfilesPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                fx.push(Fx::Deliver(MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.sel.0 }),
                    }))));
                Handled::Yes
            },
            ScreenEvent::Tick(t) => {
                let gen = control::generation();
                if gen != self.last_gen {
                    self.last_gen = gen;
                    self.state.pending = None;
                    // `reload` keeps the row the engine is on: a worker answer arriving under the
                    // cursor must never re-seat focus on the first profile.
                    self.reload(None);
                }
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            },
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) { self.state.sel = key; }
                Handled::Yes
            },
            ScreenEvent::Activate(key) => { self.activate(*key, fx); Handled::Yes },
            ScreenEvent::Input(plx_machine::machine::InputEvent { kind, .. }) => self.typing(kind, cx, fx),
            _ => Handled::No,
        }
    }
}

plx_ui::focusable_via_view!(ProfilesPage, InnerHost, view);

impl Screen<InnerHost> for ProfilesPage {
    fn name(&self) -> &'static str { super::super::registry::word::NUVIO_PROFILES }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(plx_os::i18n::msg::settings_title()))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
        // A write is still on its worker. Acknowledge the press at the row's trailing edge instead
        // of leaving the viewer to wonder whether OK registered at all.
        if let Some(id) = self.state.pending.clone() {
            if let Some(i) = self.form.index_of(&id) {
                // The table is drawn in the route's SECTIONED frame, not the whole screen: measuring
                // against FULL put the spinner two rows above the pressed one.
                let frame = RouteLayout::screen().sectioned_table();
                if let Some(r) = self.form.table.row_rect(frame, i as i32) {
                    // placeholder-exempt: an inline R_INLINE spinner marks one busy row; it is not
                    // a stand-in for content that has not arrived.
                    let env = Env::inert();
                    Spinner::new(r.x + r.w - Spinner::inline_extent() * 0.5 - theme::space::XS,
                        r.y + r.h * 0.5, Spinner::R_INLINE)
                        .phase(f.cx.tick.ms)
                        .tint(theme::TEXT_SECONDARY)
                        .draw(&env, f.painter.alpha(f.page_alpha));
                }
            }
        }
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_os::i18n::{language_on_this_thread_for_test, SHIPPED};

    fn profile(index: u32, name: &str, locked: bool) -> control::ProfileView {
        control::ProfileView { profile_index: index, name: name.into(), tv_locked: locked,
            ..Default::default() }
    }

    #[test]
    fn the_real_form_fits_every_shipped_language() {
        let profiles = vec![profile(1, "Main", false), profile(2, "Kids", true)];
        let mut out = Vec::new();
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            let mut form: FormTable<ProfId, ProfAction, SettingsPage> =
                FormTable::page(crate::registry::BAND);
            form.set(profiles_form(&profiles, 1, "Kids", false, "", "Bram", false), None);
            out.extend(form.table.app_fit_failures(
                RouteLayout::screen().sectioned_table().w, language.tag()));
        }
        plx_ui::table::assert_no_fit_failures(&out);
    }

    #[test]
    fn keyboard_rows_come_before_the_profile_list() {
        let profiles = vec![profile(1, "Main", false), profile(2, "Kids", true)];
        let form = profiles_form(&profiles, 1, "", false, "", "", false);
        let ids: Vec<ProfId> = form.slots().into_iter().flatten().map(|b| b.id.clone()).collect();
        assert_eq!(&ids[..3], &[ProfId::Rename, ProfId::PinSet, ProfId::PinClear],
            "the keyboard rows must stay in the band the LG keyboard does not cover");
        assert_eq!(ids[3], ProfId::Slot(1));
    }

    #[test]
    fn changing_the_avatar_keeps_focus_on_the_avatar_row() {
        let mut p = profile(1, "Main", false);
        p.avatar_id = Some("a".into());
        let state = ProfilesState {
            sel: RowKey(0), profiles: vec![p],
            avatars: vec![control::AvatarView { id: "a".into(), display_name: "A".into(), ..Default::default() },
                control::AvatarView { id: "b".into(), display_name: "B".into(), ..Default::default() }],
            avatar_at: 0, target: 1, name: TextBuffer::new("Main".into(), 4), editing_name: false, editing: false,
            pin: String::new(), pin_step: 0, pin_first: None,
            current_pin: String::new(), clearing: false, delete_armed: false, pending: None,
        };
        let mut page = ProfilesPage { entry: EntryId(1), form: FormTable::page(crate::registry::BAND), state, last_gen: 0 };
        page.rebuild(None, false);
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        page.activate(102, &mut fx); // ProfId::Avatar
        assert_eq!(page.form.selected_id(), Some(&ProfId::Avatar),
            "a roster write must not yank focus to the first row");
    }

    #[test]
    fn deleting_a_later_profile_seats_its_neighbour_not_profile_one() {
        let state = ProfilesState {
            sel: RowKey(0), profiles: vec![profile(1, "Main", false), profile(2, "Kids", false),
                profile(3, "Extra", false)], avatars: vec![], avatar_at: 0,
            target: 3, name: TextBuffer::new("Extra".into(), 5), editing_name: false, editing: false,
            pin: String::new(), pin_step: 0, pin_first: None,
            current_pin: String::new(), clearing: false, delete_armed: true, pending: None,
        };
        let page = ProfilesPage { entry: EntryId(1), form: FormTable::page(crate::registry::BAND), state, last_gen: 0 };
        assert_eq!(page.neighbour_after_delete(3), 2, "the row above the deleted one keeps the seat");
        assert_eq!(page.neighbour_after_delete(2), 3, "the row below when there is no above");
    }

    #[test]
    fn slot_one_cannot_arm_a_delete() {
        let form = profiles_form(&[profile(1, "Main", false)], 1, "", false, "", "", false);
        let delete = form.slots().into_iter().flatten().find(|b| b.id == ProfId::Delete).unwrap();
        assert!(delete.disabled, "the API refuses to delete slot 1; do not offer a stuck confirmation");
    }

    #[test]
    fn forgot_pin_raises_the_qr_sign_in() {
        let state = ProfilesState {
            sel: RowKey(0), profiles: vec![profile(1, "Main", true)], avatars: vec![], avatar_at: 0,
            target: 1, name: TextBuffer::new("Main".into(), 4), editing_name: false, editing: false,
            pin: String::new(), pin_step: 0, pin_first: None,
            current_pin: String::new(), clearing: false, delete_armed: false, pending: None,
        };
        let mut page = ProfilesPage { entry: EntryId(1), form: FormTable::page(crate::registry::BAND), state, last_gen: 0 };
        page.rebuild(None, false);
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        page.activate(107, &mut fx); // ProfId::ForgotPin
        assert!(out.iter().any(|s| matches!(&s.fx,
            Fx::App(AppFx::Session(plx_session::auth::SessionCmd::StartLogin)))),
            "the reset row must open the QR sign-in, not just arm the shim");
    }

    #[test]
    fn a_worker_snapshot_keeps_the_row_under_the_cursor() {
        let profiles = vec![profile(1, "Main", false), profile(2, "Kids", false)];
        let state = ProfilesState {
            sel: RowKey(0), profiles: profiles.clone(), avatars: vec![], avatar_at: 0,
            target: 1, name: TextBuffer::new("Main".into(), 4), editing_name: false, editing: false,
            pin: String::new(), pin_step: 0, pin_first: None,
            current_pin: String::new(), clearing: false, delete_armed: false, pending: None,
        };
        let mut page = ProfilesPage { entry: EntryId(1), form: FormTable::page(crate::registry::BAND), state, last_gen: 0 };
        page.rebuild(None, false);
        page.rebuild(Some(ProfId::Slot(2)), false);
        assert_eq!(page.form.selected_id(), Some(&ProfId::Slot(2)));
        // A worker refresh lands with no explicit `keep`: it must not re-seat focus on profile 1.
        page.apply_snapshot(profiles, vec![], None);
        assert_eq!(page.form.selected_id(), Some(&ProfId::Slot(2)));
    }

    #[test]
    fn a_wrong_current_pin_never_clears_a_lock() {
        let state = ProfilesState {
            sel: RowKey(0), profiles: vec![profile(2, "Parent", true)], avatars: vec![], avatar_at: 0,
            target: 2, name: TextBuffer::new("Parent".into(), 6), editing_name: false, editing: false,
            pin: "0000".into(), pin_step: 1, pin_first: None,
            current_pin: String::new(), clearing: true, delete_armed: false, pending: None,
        };
        let mut page = ProfilesPage { entry: EntryId(1), form: FormTable::page(crate::registry::BAND), state, last_gen: 0 };
        // With no control registered the write is refused, which must fail CLOSED: a wrong
        // or uncheckable PIN leaves the lock exactly as it was.
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        page.commit_pin_digits(&mut fx);
        assert!(page.state.profiles[0].tv_locked);
        assert_eq!(page.state.pin_step, 0);
        assert!(page.state.pin.is_empty());
    }

    #[test]
    fn set_tv_pin_raises_the_keyboard() {
        let state = ProfilesState {
            sel: RowKey(0), profiles: vec![profile(1, "Main", false)], avatars: vec![], avatar_at: 0,
            target: 1, name: TextBuffer::new("Main".into(), 4), editing_name: false, editing: false,
            pin: String::new(), pin_step: 0, pin_first: None,
            current_pin: String::new(), clearing: false, delete_armed: false, pending: None,
        };
        let mut page = ProfilesPage { entry: EntryId(1), form: FormTable::page(crate::registry::BAND), state, last_gen: 0 };
        page.rebuild(None, false);
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        page.activate(104, &mut fx);
        assert!(out.iter().any(|s| matches!(s.fx, Fx::Deliver(_, Delivery::Keyboard { up: true }))),
            "the LG sends no text edits until the page raises its keyboard");
        assert!(page.state.editing);
        assert_eq!(page.state.pin_step, 2);
    }

    #[test]
    fn a_locked_profile_editor_never_sends_pin_fields() {
        let state = ProfilesState {
            sel: RowKey(0), profiles: vec![profile(2, "Parent", true)], avatars: vec![], avatar_at: 0,
            target: 2, name: TextBuffer::new("Parent".into(), 6), editing_name: true, editing: false,
            pin: String::new(), pin_step: 0, pin_first: None,
            current_pin: String::new(), clearing: false, delete_armed: false, pending: None,
        };
        let page = ProfilesPage { entry: EntryId(1), form: FormTable::page(crate::registry::BAND), state, last_gen: 0 };
        let edits = page.edits();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].name, "Parent");
        assert_eq!(edits[0].profile_index, 2);
    }
}
