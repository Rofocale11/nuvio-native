use std::borrow::Cow;

use plx_machine::machine::{
    Canon, Cx, Delivery, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InstanceId, Key,
    LogicalState, Machine, MachineId, ScreenEvent,
};
use plx_machine::present::Provenance;
use plx_ui::form::{Form, FormId, FormSection, FormTable, RowKey, RowKind};
use plx_ui::frame::Budget;
use plx_ui::route_screen::RouteLayout;
use plx_ui::screen::{DrawFrame, Enter, FocusSource, FocusTarget, HitSource, RenderStrategy, Screen};
use plx_ui::table::Row;
use plx_ui::table_screen::{Header, TableScreen};
use plx_ui::{theme, Rect};

use super::family::{form_activate, form_focus, form_right_target, InnerHost, SettingsPage};

#[derive(Clone, PartialEq, Eq, Debug)]
enum AddonId {
    Row(usize),
}

impl FormId for AddonId {
    fn key(&self) -> RowKey {
        RowKey(match self { AddonId::Row(i) => *i as u32 })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum AddonAction {
    Toggle(usize),
}

fn addons_form(addons: &[(String, String, bool)]) -> Form<AddonId, AddonAction, SettingsPage> {
    let mut section = FormSection::new("");
    for (i, (_key, name, enabled)) in addons.iter().enumerate() {
        section = section.item(
            AddonId::Row(i),
            RowKind::Toggle,
            AddonAction::Toggle(i),
            Row::new(name).toggle(*enabled),
        );
    }
    Form::new().section(section)
}

struct AddonsState {
    sel: RowKey,
    addons: Vec<(String, String, bool)>,
}

impl LogicalState for AddonsState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0);
        w.u32(self.addons.len() as u32);
        for (k, n, e) in &self.addons {
            w.str(k);
            w.str(n);
            w.bool(*e);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("addons sel={} count={}", self.sel.0, self.addons.len()));
    }
}

pub struct AddonsPage {
    entry: EntryId,
    form: FormTable<AddonId, AddonAction, SettingsPage>,
    state: AddonsState,
}

impl AddonsPage {
    pub fn new(entry: EntryId) -> Self {
        let addons = plx_plex::plex::nuvio::installed_addons();
        let mut page = Self {
            entry,
            form: FormTable::new(super::registry::BAND),
            state: AddonsState { sel: RowKey(0), addons },
        };
        page.rebuild(None);
        page
    }

    fn rebuild(&mut self, keep: Option<AddonId>) {
        let addons = plx_plex::plex::nuvio::installed_addons();
        self.state.addons = addons;
        self.form.table.compact = false;
        self.form.table.header_ink = theme::TEXT_READING;
        self.form.table.list_focused = true;
        self.form.set(addons_form(&self.state.addons), keep.as_ref());
        self.state.sel = self.form.key_at(self.form.table.sel.max(0) as usize).map_or(RowKey(0), |k| k);
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(RouteLayout::screen(), Some(plx_platform::i18n::msg::settings_title()),
                plx_platform::i18n::msg::settings_addons_title(), plx_platform::i18n::msg::settings_addons_detail()),
            &self.form.table, GroupId(0), self.entry,
        ).keyed(&self.form)
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        if let Some(AddonAction::Toggle(i)) = form_activate(&self.form, key, fx) {
            if i < self.state.addons.len() {
                let (k, _name, enabled) = &self.state.addons[i];
                let _ = plx_plex::plex::nuvio::set_addon_enabled(k.clone(), !*enabled);
                let keep = self.form.selected_id().cloned();
                self.rebuild(keep);
                fx.invalidate(Provenance::Input);
            }
        }
    }
}

impl Machine<InnerHost> for AddonsPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.sel.0 }),
                    })),
                ));
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) { self.state.sel = key; }
                Handled::Yes
            }
            ScreenEvent::Activate(key) => { self.activate(*key, fx); Handled::Yes }
            ScreenEvent::Input(plx_machine::machine::InputEvent { kind: plx_machine::machine::InputKind::Key {
                key: Key::Right, at_edge: true, .. }, .. }) => {
                if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                    self.activate(key, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

plx_ui::focusable_via_view!(AddonsPage, InnerHost, view);

impl Screen<InnerHost> for AddonsPage {
    fn name(&self) -> &'static str { "addons" }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(plx_platform::i18n::msg::settings_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}
