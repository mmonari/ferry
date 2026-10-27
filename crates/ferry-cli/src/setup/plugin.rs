//! The bar-plugin step of `ferry setup`: links the packaged
//! plugin into the shell's plugin directory and puts its widget on the bar.
//!
//! Two files are involved, and the shell owns both of them:
//!
//! - `~/.config/omarchy/plugins/ferry.bar` is a symlink to the package's plugin folder
//!   (`/usr/share/ferry/plugin`), the way `jankeesvw.meeting-recorder` is linked on an Omarchy
//!   machine. A link or a directory that is not ours is the user's, for instance a checkout
//!   under development, and is never touched.
//! - `~/.config/omarchy/shell.json` says which widgets are on the bar. A plugin is "enabled"
//!   when its id is in `bar.layout.<section>` (`PluginRegistry.qml`, `isEnabled`), and a valid
//!   `shell.json` replaces the shell's default one entirely, with no merge. So the step edits
//!   the user's file where it exists, starts a missing one from the shell's default (a file with
//!   only ferry in it would wipe the user's bar), and leaves a file the shell would not read
//!   alone. Where the shell's own `setEnabled` would put a widget with no placement (the
//!   manifest's default section, after the tray or the clone that took the tray's place), so
//!   does this, and it takes the id out of `disabledPlugins`, which the shell does too.
//!
//! The edit is made on the text (`jsonedit`), not by writing the parsed file back: the shell
//! keeps its keys in the order they were written, and a rewrite would show the user a diff they
//! did not ask for. The result is parsed again and compared with the value it must have before
//! it is written.
//!
//! A cloned bar (`bar.id` is `my.bar`) changes nothing here: every bar reads `bar.layout`.
//! Clones show up in the ids inside the layout (`my.tray` for `omarchy.tray`), which is what the
//! placement looks through.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use super::fsx::{self, ReadError};
use super::{Env, Out, Ui, jsonedit};

/// The plugin's id, as its `manifest.json` and the layout say it.
pub const PLUGIN_ID: &str = "ferry.bar";
const TITLE: &str = "bar plugin (ferry.bar: the widget and panels in omarchy-shell)";
/// The shell's own way to do what this step does, for the user whose files it will not edit.
const ENABLE: &str = "omarchy plugin enable ferry.bar --section right";
const DISABLE: &str = "omarchy plugin disable ferry.bar";
/// The sections of `bar.layout`, in the shell's order (`Util.barSections`).
const SECTIONS: [&str; 6] = [
    "left",
    "centerBegin",
    "center",
    "centerEnd",
    "right",
    "tray",
];
/// Where the plugin goes when its manifest does not say.
const FALLBACK_SECTION: &str = "right";

// --- what shell.json says --------------------------------------------------------------------

/// Why a `shell.json` is not one this step edits.
#[derive(Debug, thiserror::Error)]
pub enum ShellJsonError {
    #[error("is not valid JSON ({0})")]
    Json(String),
    #[error("is not a JSON object")]
    NotObject,
    /// The shell ignores a file without `"version": 1`.
    #[error("has no \"version\": 1")]
    Version,
    #[error("has no bar.layout")]
    NoLayout,
    #[error("has no bar.layout.{0}")]
    NoSection(String),
    /// The edit did not come out as it should, so nothing is written.
    #[error("could not be edited with certainty ({0})")]
    Edit(&'static str),
}

/// Where the widget went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub section: String,
    pub index: usize,
    /// The element it follows, when there is one to follow.
    pub after: Option<String>,
}

/// `shell.json` with the widget on the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    pub text: String,
    /// `None`: the widget was on the bar already.
    pub placed: Option<Placed>,
    /// The id was in `disabledPlugins`, which makes the shell treat the plugin as off.
    pub reenabled: bool,
}

/// The file's value, if it is one the shell reads: an object with `"version": 1` and a
/// `bar.layout` object (`shell.qml`, `applyShellConfig`).
fn check(text: &str) -> Result<Value, ShellJsonError> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| ShellJsonError::Json(e.to_string()))?;
    if !value.is_object() {
        return Err(ShellJsonError::NotObject);
    }
    if value.get("version").and_then(Value::as_f64) != Some(1.0) {
        return Err(ShellJsonError::Version);
    }
    match value.get("bar").and_then(|b| b.get("layout")) {
        Some(Value::Object(_)) => Ok(value),
        _ => Err(ShellJsonError::NoLayout),
    }
}

/// The id of a layout entry, which is a string or an object with an `id` (the shell takes both).
fn element_id(entry: &Value) -> Option<&str> {
    entry
        .as_str()
        .or_else(|| entry.get("id").and_then(Value::as_str))
}

fn section_of<'a>(value: &'a Value, section: &str) -> &'a [Value] {
    value
        .get("bar")
        .and_then(|b| b.get("layout"))
        .and_then(|l| l.get(section))
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
}

fn section_mut<'a>(value: &'a mut Value, section: &str) -> Option<&'a mut Vec<Value>> {
    value
        .get_mut("bar")?
        .get_mut("layout")?
        .get_mut(section)?
        .as_array_mut()
}

/// The positions of the widget in a section.
fn widget_at(value: &Value, section: &str) -> Vec<usize> {
    section_of(value, section)
        .iter()
        .enumerate()
        .filter(|(_, e)| element_id(e) == Some(PLUGIN_ID))
        .map(|(i, _)| i)
        .collect()
}

/// The positions of the id in `disabledPlugins`.
fn disabled_at(value: &Value) -> Vec<usize> {
    value
        .get("disabledPlugins")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .enumerate()
                .filter(|(_, v)| v.as_str() == Some(PLUGIN_ID))
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}

/// Where the shell puts a widget enabled with no placement (`PluginRegistry.qml`, `barTarget`):
/// right after the element the section is anchored on, or the plugin that took its place (a
/// clone), else at the end. `is_clone_of(id, source)` says whether plugin `id` is a clone of
/// `source`.
fn placement(
    section: &str,
    list: &[Value],
    is_clone_of: &dyn Fn(&str, &str) -> bool,
) -> (usize, Option<String>) {
    let anchor = match section {
        "left" => Some("omarchy.workspaces"),
        "centerEnd" => Some("omarchy.weather"),
        "right" => Some("omarchy.tray"),
        _ => None,
    };
    if let Some(anchor) = anchor {
        let ids: Vec<Option<&str>> = list.iter().map(element_id).collect();
        let at = ids.iter().position(|id| *id == Some(anchor)).or_else(|| {
            ids.iter()
                .position(|id| id.is_some_and(|id| is_clone_of(id, anchor)))
        });
        if let Some(at) = at {
            return (at + 1, ids.get(at).copied().flatten().map(str::to_string));
        }
    }
    (list.len(), None)
}

/// The text of `shell.json` with the widget on the bar in `section`, and out of `disabledPlugins`.
/// `Ok(None)` when it is on the bar already and not disabled.
pub fn add_widget(
    text: &str,
    section: &str,
    is_clone_of: &dyn Fn(&str, &str) -> bool,
) -> Result<Option<Added>, ShellJsonError> {
    let value = check(text)?;
    let on_bar = SECTIONS.iter().any(|s| !widget_at(&value, s).is_empty());
    let disabled = disabled_at(&value);
    if on_bar && disabled.is_empty() {
        return Ok(None);
    }

    let mut out = text.to_string();
    let mut expected = value.clone();
    let mut placed = None;
    if !on_bar {
        let list = value
            .get("bar")
            .and_then(|b| b.get("layout"))
            .and_then(|l| l.get(section))
            .and_then(Value::as_array)
            .ok_or_else(|| ShellJsonError::NoSection(section.to_string()))?;
        let (index, after) = placement(section, list, is_clone_of);
        let array = jsonedit::array_at(&out, &["bar", "layout", section])
            .filter(|a| a.elements.len() == list.len())
            .ok_or(ShellJsonError::Edit("the array is not where it was parsed"))?;
        let inserted = jsonedit::insert_id(&out, &array, index, PLUGIN_ID);
        out = inserted.text;
        let widget = if inserted.bare_string {
            Value::String(PLUGIN_ID.to_string())
        } else {
            json!({ "id": PLUGIN_ID })
        };
        section_mut(&mut expected, section)
            .ok_or(ShellJsonError::Edit("the section is gone"))?
            .insert(index, widget);
        placed = Some(Placed {
            section: section.to_string(),
            index,
            after,
        });
    }
    // later positions first, so that the earlier ones stay where they were parsed
    for &i in disabled.iter().rev() {
        let array = jsonedit::array_at(&out, &["disabledPlugins"]).ok_or(ShellJsonError::Edit(
            "disabledPlugins is not where it was parsed",
        ))?;
        out = jsonedit::remove(&out, &array, i)
            .ok_or(ShellJsonError::Edit("an entry of disabledPlugins is gone"))?;
    }
    if let Some(list) = expected
        .get_mut("disabledPlugins")
        .and_then(Value::as_array_mut)
    {
        list.retain(|v| v.as_str() != Some(PLUGIN_ID));
    }
    verify(&out, &expected)?;
    Ok(Some(Added {
        text: out,
        placed,
        reenabled: !disabled.is_empty(),
    }))
}

/// The text of `shell.json` without the widget, and how many entries went. `Ok(None)` when it is
/// not on the bar.
pub fn remove_widget(text: &str) -> Result<Option<(String, usize)>, ShellJsonError> {
    let value = check(text)?;
    let mut out = text.to_string();
    let mut expected = value.clone();
    let mut count = 0;
    for section in SECTIONS {
        for &i in widget_at(&value, section).iter().rev() {
            let array = jsonedit::array_at(&out, &["bar", "layout", section])
                .ok_or(ShellJsonError::Edit("the array is not where it was parsed"))?;
            out = jsonedit::remove(&out, &array, i)
                .ok_or(ShellJsonError::Edit("an entry of the layout is gone"))?;
            count += 1;
        }
        if let Some(list) = section_mut(&mut expected, section) {
            list.retain(|e| element_id(e) != Some(PLUGIN_ID));
        }
    }
    if count == 0 {
        return Ok(None);
    }
    verify(&out, &expected)?;
    Ok(Some((out, count)))
}

/// The edited text is the value it was meant to be: the one the file had, plus or minus the one
/// entry. A bug in the scan must not reach the user's file.
fn verify(text: &str, expected: &Value) -> Result<(), ShellJsonError> {
    match serde_json::from_str::<Value>(text) {
        Ok(got) if got == *expected => Ok(()),
        Ok(_) => Err(ShellJsonError::Edit("the result is not what it should be")),
        Err(_) => Err(ShellJsonError::Edit("the result is not JSON")),
    }
}

/// The lines that differ between `old` and `new` (the lines both share at either end are the
/// context, `context` of them on each side), each with its marker: `+ added`, `- removed`, and
/// two spaces for context.
///
/// A block that was only added or only removed can often be drawn at more than one place: the
/// line before it may equal its own last line (`},` `{` `"id": "x"` `},` `{`). It is drawn where it
/// starts at the shallowest indentation, so that an object added to a list reads as an object.
pub fn change_lines(old: &str, new: &str, context: usize) -> Vec<String> {
    let o: Vec<&str> = old.lines().collect();
    let n: Vec<&str> = new.lines().collect();
    let head = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let tail = o
        .iter()
        .skip(head)
        .rev()
        .zip(n.iter().skip(head).rev())
        .take_while(|(a, b)| a == b)
        .count();
    if head == o.len() && head == n.len() {
        return Vec::new();
    }
    let (o_end, n_end) = (o.len() - tail, n.len() - tail);
    let up = if o_end == head {
        slide_up(&n, head, n_end)
    } else if n_end == head {
        slide_up(&o, head, o_end)
    } else {
        0
    };
    let (head, o_end, n_end) = (head - up, o_end - up, n_end - up);

    let lines = |all: &[&str], from: usize, to: usize, marker: &str| -> Vec<String> {
        all.get(from..to)
            .unwrap_or_default()
            .iter()
            .map(|l| format!("{marker} {l}"))
            .collect()
    };
    let mut out = lines(&o, head.saturating_sub(context), head, " ");
    out.extend(lines(&o, head, o_end, "-"));
    out.extend(lines(&n, head, n_end, "+"));
    out.extend(lines(&n, n_end, (n_end + context).min(n.len()), " "));
    out
}

/// How far the block `lines[from..to]` may move up and still be the same change, and still read
/// best: the distance at which its first line is the least indented, the nearest one on a tie.
fn slide_up(lines: &[&str], from: usize, to: usize) -> usize {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut best: Option<(usize, usize)> = None;
    for up in 0..=from {
        if let Some(first) = lines.get(from - up) {
            let depth = indent(first);
            if best.is_none_or(|(d, _)| depth < d) {
                best = Some((depth, up));
            }
        }
        // one more step up: the line above must be the block's last line
        let above = from.checked_sub(up + 1).and_then(|i| lines.get(i));
        let last = to.checked_sub(up + 1).and_then(|i| lines.get(i));
        if above.is_none() || above != last {
            break;
        }
    }
    best.map_or(0, |(_, up)| up)
}

// --- what is on the machine ----------------------------------------------------------------

/// What `~/.config/omarchy/plugins/ferry.bar` is.
enum Link {
    Absent,
    /// A symlink to the package's plugin folder: setup's.
    Ours,
    /// The user's: a link to somewhere else, a directory, a file. The words that follow
    /// "ferry.bar is".
    Foreign(String),
}

fn link_state(link: &Path, source: &Path) -> io::Result<Link> {
    let meta = match fs::symlink_metadata(link) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Link::Absent),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_symlink() {
        return Ok(Link::Foreign(
            if meta.is_dir() {
                "a directory"
            } else {
                "a file"
            }
            .into(),
        ));
    }
    let target = fs::read_link(link)?;
    // a relative target is relative to the directory the link is in
    let absolute = link
        .parent()
        .map_or_else(|| target.clone(), |d| d.join(&target));
    let same = absolute == source
        || matches!(
            (fs::canonicalize(&absolute), fs::canonicalize(source)),
            (Ok(a), Ok(b)) if a == b
        );
    Ok(if same {
        Link::Ours
    } else {
        Link::Foreign(format!("linked to {}", target.display()))
    })
}

/// The section the plugin's manifest asks for (`barWidget.defaultSection`), as the shell reads
/// it: `center` means `centerEnd`.
fn default_section(source: &Path) -> &'static str {
    let wanted = fsx::read_text(&source.join("manifest.json"))
        .ok()
        .flatten()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|m| {
            m.get("barWidget")?
                .get("defaultSection")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default();
    let wanted = if wanted == "center" {
        "centerEnd"
    } else {
        wanted.as_str()
    };
    SECTIONS
        .iter()
        .find(|s| **s == wanted)
        .copied()
        .unwrap_or(FALLBACK_SECTION)
}

/// A plugin id the shell would accept as a folder name: nothing in it climbs out of the plugin
/// directory.
fn is_plain_id(id: &str) -> bool {
    id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !id.contains("..")
}

/// Whether the plugin `id` in the user's plugin directory is a clone of `source` (its manifest says
/// `omarchy.clonedFrom`). A manifest that is missing or unreadable says no.
fn is_clone_in(plugins: &Path) -> impl Fn(&str, &str) -> bool + '_ {
    move |id, source| {
        is_plain_id(id)
            && fsx::read_text(&plugins.join(id).join("manifest.json"))
                .ok()
                .flatten()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .is_some_and(|m| {
                    m.get("omarchy")
                        .and_then(|o| o.get("clonedFrom"))
                        .and_then(Value::as_str)
                        == Some(source)
                })
    }
}

/// The change to `shell.json`, ready to show and to write.
struct LayoutEdit {
    /// The file to write: a dotfiles symlink is followed.
    target: PathBuf,
    /// What it holds now. `None`: it does not exist, and is created from the shell's defaults.
    before: Option<String>,
    /// The text the change is shown against: the file, or the defaults it starts from.
    shown_against: String,
    after: String,
    /// The defaults a new file starts from.
    seeded_from: Option<PathBuf>,
    /// The sentence that goes in the plan, and the one that goes in the result.
    plan: String,
    done: String,
}

enum Layout {
    /// Nothing to change.
    Fine,
    Edit(Box<LayoutEdit>),
    /// The file is not one setup edits; the words say why, starting with its name.
    Unusable(String),
    /// The file could not be read.
    Failed(String),
}

fn read_shell_json(path: &Path, shown: &str) -> Result<Option<String>, Layout> {
    match fsx::read_text(path) {
        Ok(t) => Ok(t),
        Err(ReadError::NotUtf8) => Err(Layout::Unusable(format!("{shown} is not valid UTF-8"))),
        Err(e) => Err(Layout::Failed(format!("{shown}: {e}"))),
    }
}

fn where_text(p: &Placed) -> String {
    match &p.after {
        Some(after) => format!("{}, after {after}", p.section),
        None => format!("{}, at the end", p.section),
    }
}

fn plan_add(ui: &Ui, env: &Env) -> Layout {
    let user = env.shell_json();
    let shown = ui.tilde(&user);
    let target = fsx::resolve(&user);
    let plugins = env.omarchy_dir().join("plugins");
    let clone = is_clone_in(&plugins);
    let section = default_section(&env.plugin_source);

    let (base, seeded_from) = match read_shell_json(&target, &shown) {
        Ok(Some(text)) => (text, None),
        Ok(None) => {
            // No file of the user's: the shell runs on its default, and a file we write replaces
            // it whole. Start from the default, or leave the choice to the user.
            let defaults = env.omarchy_path.join("config/omarchy/shell.json");
            let there = ui.tilde(&defaults);
            match read_shell_json(&defaults, &there) {
                Ok(Some(text)) => (text, Some(defaults)),
                Ok(None) => {
                    return Layout::Unusable(format!(
                        "{shown} does not exist, and the shell's default {there} is not there to start it from"
                    ));
                }
                Err(Layout::Failed(_)) => {
                    return Layout::Unusable(format!(
                        "{shown} does not exist, and {there} cannot be read"
                    ));
                }
                Err(other) => return other,
            }
        }
        Err(other) => return other,
    };
    let from = if seeded_from.is_some() {
        ui.tilde(&env.omarchy_path.join("config/omarchy/shell.json"))
    } else {
        shown.clone()
    };
    match add_widget(&base, section, &clone) {
        Ok(None) => Layout::Fine,
        Ok(Some(added)) => {
            let (mut plan, mut done) = (Vec::new(), Vec::new());
            if let Some(p) = &added.placed {
                plan.push(format!("add the widget to the bar: {}", where_text(p)));
                done.push(format!("put the widget on the bar ({})", where_text(p)));
            }
            if added.reenabled {
                plan.push("take ferry.bar out of disabledPlugins".to_string());
                done.push("took ferry.bar out of disabledPlugins".to_string());
            }
            Layout::Edit(Box::new(LayoutEdit {
                target,
                before: seeded_from.is_none().then(|| base.clone()),
                shown_against: base,
                after: added.text,
                seeded_from,
                plan: plan.join("; "),
                done: done.join(" and "),
            }))
        }
        Err(e) => Layout::Unusable(format!("{from} {e}")),
    }
}

fn plan_remove(ui: &Ui, env: &Env) -> Layout {
    let user = env.shell_json();
    let shown = ui.tilde(&user);
    let target = fsx::resolve(&user);
    let text = match fsx::read_text(&target) {
        Ok(Some(text)) => text,
        Ok(None) => return Layout::Fine,
        // not text, so not a file the shell reads; it matters only if the id is in it
        Err(ReadError::NotUtf8) => {
            let names_it = fs::read(&target).is_ok_and(|b| {
                b.windows(PLUGIN_ID.len())
                    .any(|w| w == PLUGIN_ID.as_bytes())
            });
            return if names_it {
                Layout::Unusable(format!("{shown} is not valid UTF-8"))
            } else {
                Layout::Fine
            };
        }
        Err(e) => return Layout::Failed(format!("{shown}: {e}")),
    };
    // a file that never says the id has nothing of ours in it, whatever else it is
    if !text.contains(PLUGIN_ID) {
        return Layout::Fine;
    }
    match remove_widget(&text) {
        Ok(None) => Layout::Fine,
        Ok(Some((after, _))) => Layout::Edit(Box::new(LayoutEdit {
            target,
            before: Some(text.clone()),
            shown_against: text,
            after,
            seeded_from: None,
            plan: "take the widget off the bar".into(),
            done: "took the widget off the bar".into(),
        })),
        Err(e) => Layout::Unusable(format!("{shown} {e}")),
    }
}

// --- the step ----------------------------------------------------------------------------------

/// Asks the shell to look at its plugin directory again. Best effort: the shell may not be
/// running (it finds the link when it starts), and nothing here depends on the answer.
fn rescan(env: &Env) {
    let _ = Command::new(&env.omarchy_shell)
        .args(["-q", "shell", "rescanPlugins"])
        .env("OMARCHY_PATH", &env.omarchy_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn show_edit(ui: &mut Ui, env: &Env, edit: &LayoutEdit) {
    let mut label = ui.tilde(&edit.target);
    if let Some(defaults) = &edit.seeded_from {
        label = format!(
            "{label} (new file from the shell's default {}; {})",
            ui.tilde(defaults),
            edit.plan
        );
    } else {
        label = format!("{label} ({}", edit.plan);
        if edit.target != env.shell_json() {
            label.push_str("; the link points here, the link itself stays");
        }
        label.push(')');
    }
    ui.say(format!("  --- {label}"));
    for line in change_lines(&edit.shown_against, &edit.after, 2) {
        ui.say(format!("  {line}"));
    }
}

fn write_edit(ui: &mut Ui, env: &Env, edit: &LayoutEdit) -> Result<(), String> {
    if edit.before.is_some() {
        match fsx::backup(&edit.target, env.unix_time) {
            Ok(b) => ui.say(format!("  backup: {}", ui.tilde(&b))),
            Err(e) => {
                return Err(format!("backup of {}: {e}", ui.tilde(&edit.target)));
            }
        }
    }
    fsx::write_atomic(&edit.target, edit.after.as_bytes())
        .map_err(|e| format!("{}: {e}", ui.tilde(&edit.target)))
}

pub(super) fn step(ui: &mut Ui, env: &Env) {
    ui.header(TITLE);
    let out = add(ui, env);
    ui.finish(out);
}

fn add(ui: &mut Ui, env: &Env) -> Out {
    let link_path = env.plugin_link();
    let link = match link_state(&link_path, &env.plugin_source) {
        Ok(l) => l,
        Err(e) => return Out::Failed(format!("{}: {e}", ui.tilde(&link_path))),
    };
    let packaged = env.plugin_source.join("manifest.json").is_file();
    // without the package there is nothing to link, and nothing to put on the bar
    if !packaged && !matches!(link, Link::Foreign(_)) {
        ui.say(format!(
            "  {} is not there: the ferry package is not installed (a build run from a directory has no plugin folder).",
            ui.tilde(&env.plugin_source)
        ));
        ui.say("  From a checkout, link its plugin folder and enable it:");
        ui.say(format!(
            "    $ ln -s <checkout>/plugin {}",
            ui.tilde(&link_path)
        ));
        ui.say(format!("    $ {ENABLE}"));
        return Out::Printed("no packaged plugin to link".into());
    }
    let edit = match plan_add(ui, env) {
        Layout::Fine => None,
        Layout::Edit(e) => Some(e),
        Layout::Unusable(why) => {
            ui.say(format!(
                "  {why}; setup leaves it alone. Put the widget on the bar yourself:"
            ));
            if matches!(link, Link::Absent) {
                ui.say(format!(
                    "    $ ln -s {} {}",
                    ui.tilde(&env.plugin_source),
                    ui.tilde(&link_path)
                ));
            }
            ui.say(format!("    $ {ENABLE}"));
            return Out::Printed("shell.json is not one setup edits".into());
        }
        Layout::Failed(why) => return Out::Failed(why),
    };
    let make_link = matches!(link, Link::Absent);
    if !make_link && edit.is_none() {
        return Out::Skipped(match &link {
            Link::Foreign(what) => {
                format!("ferry.bar is on the bar, from a plugin folder that is {what} (yours)")
            }
            _ => "ferry.bar is linked and on the bar".into(),
        });
    }

    if let Link::Foreign(what) = &link {
        ui.say(format!("  ferry.bar is {what} (yours), left alone."));
    }
    ui.say("  will do:");
    if make_link {
        ui.say(format!(
            "  $ ln -s {} {}",
            ui.tilde(&env.plugin_source),
            ui.tilde(&link_path)
        ));
        ui.say("  $ omarchy-shell -q shell rescanPlugins");
    }
    if let Some(e) = &edit {
        show_edit(ui, env, e);
    }
    match ui.confirm("apply?", true) {
        None => return Out::Pending,
        Some(false) => {
            ui.say(format!(
                "  (to do it yourself: link the plugin folder to {}, then run `{ENABLE}`)",
                ui.tilde(&link_path)
            ));
            return Out::Declined;
        }
        Some(true) => {}
    }

    // The link first, so that the shell knows the plugin before the layout names it.
    if make_link {
        if let Some(dir) = link_path.parent()
            && let Err(e) = fs::create_dir_all(dir)
        {
            return Out::Failed(format!("{}: {e}", ui.tilde(dir)));
        }
        if let Err(e) = symlink(&env.plugin_source, &link_path) {
            return Out::Failed(format!("{}: {e}", ui.tilde(&link_path)));
        }
        rescan(env);
    }
    if let Some(e) = &edit
        && let Err(why) = write_edit(ui, env, e)
    {
        return Out::Failed(if make_link {
            format!("{why} (the plugin is linked; put the widget on the bar with `{ENABLE}`)")
        } else {
            why
        });
    }
    let mut parts = Vec::new();
    if make_link {
        parts.push("linked ferry.bar".to_string());
    }
    if let Some(e) = &edit {
        parts.push(e.done.clone());
    }
    Out::Done(parts.join(" and "))
}

pub(super) fn undo(ui: &mut Ui, env: &Env) {
    ui.header(TITLE);
    let out = take_away(ui, env);
    ui.finish(out);
}

fn take_away(ui: &mut Ui, env: &Env) -> Out {
    let link_path = env.plugin_link();
    let link = match link_state(&link_path, &env.plugin_source) {
        Ok(l) => l,
        Err(e) => return Out::Failed(format!("{}: {e}", ui.tilde(&link_path))),
    };
    let ours = matches!(link, Link::Ours);
    let edit = match plan_remove(ui, env) {
        Layout::Fine => None,
        Layout::Edit(e) => Some(e),
        Layout::Unusable(why) => {
            ui.say(format!(
                "  {why}; setup leaves it alone. Take the widget off the bar yourself:"
            ));
            ui.say(format!("    $ {DISABLE}"));
            if ours {
                ui.say(format!("    $ rm {}", ui.tilde(&link_path)));
            }
            return Out::Printed("shell.json is not one setup edits".into());
        }
        Layout::Failed(why) => return Out::Failed(why),
    };
    if edit.is_none() && !ours {
        return Out::Skipped(match &link {
            Link::Foreign(what) => {
                format!("ferry.bar is not on the bar, and the plugin folder is {what} (yours)")
            }
            _ => "ferry.bar is not on the bar and setup left no link".into(),
        });
    }
    if let Link::Foreign(what) = &link {
        ui.say(format!("  ferry.bar is {what} (yours), left alone."));
    }
    ui.say("  will do:");
    if let Some(e) = &edit {
        show_edit(ui, env, e);
    }
    if ours {
        ui.say(format!("  $ rm {}", ui.tilde(&link_path)));
        ui.say("  $ omarchy-shell -q shell rescanPlugins");
    }
    match ui.confirm("apply?", true) {
        None => return Out::Pending,
        Some(false) => return Out::Declined,
        Some(true) => {}
    }

    // The layout first, so that the shell never names a plugin it has lost.
    if let Some(e) = &edit
        && let Err(why) = write_edit(ui, env, e)
    {
        return Out::Failed(why);
    }
    if ours {
        if let Err(e) = fs::remove_file(&link_path) {
            return Out::Failed(format!("{}: {e}", ui.tilde(&link_path)));
        }
        rescan(env);
    }
    let mut parts = Vec::new();
    if let Some(e) = &edit {
        parts.push(e.done.clone());
    }
    if ours {
        parts.push("removed the link".to_string());
    }
    Out::Done(parts.join(" and "))
}

// --- ferry doctor ---------------------------------------------------------------------------

/// The `plugin` row of `ferry doctor` (D-2): is `ferry.bar` linked, and is its widget on the bar.
/// It reads the same files setup writes, and changes nothing.
pub fn doctor_check(env: &Env) -> ferry_core::doctor::Check {
    use ferry_core::doctor::Check;
    const NAME: &str = "plugin";
    let link_path = env.plugin_link();
    let linked = match link_state(&link_path, &env.plugin_source) {
        Ok(Link::Absent) => {
            return Check::warn(
                NAME,
                format!(
                    "{} is not linked: the bar has no ferry widget",
                    link_path.display()
                ),
                "ferry setup",
            );
        }
        Ok(Link::Ours) => "linked".to_string(),
        Ok(Link::Foreign(what)) => format!("{PLUGIN_ID} is {what}"),
        Err(e) => return Check::warn(NAME, format!("{}: {e}", link_path.display()), ""),
    };
    let user = fsx::resolve(&env.shell_json());
    let text = match fsx::read_text(&user) {
        Ok(Some(t)) => Some(t),
        Ok(None) => fsx::read_text(&env.omarchy_path.join("config/omarchy/shell.json"))
            .ok()
            .flatten(),
        Err(_) => None,
    };
    let Some(value) = text.as_deref().and_then(|t| check(t).ok()) else {
        return Check::warn(
            NAME,
            format!("{linked}, but no shell.json the shell reads says where the widget goes"),
            ENABLE,
        );
    };
    let section = SECTIONS.iter().find(|s| !widget_at(&value, s).is_empty());
    match (section, disabled_at(&value).is_empty()) {
        (Some(s), true) => Check::pass(NAME, format!("{linked}, on the bar ({s})")),
        (Some(_), false) => Check::warn(
            NAME,
            format!("{linked}, but {PLUGIN_ID} is in disabledPlugins"),
            ENABLE,
        ),
        (None, _) => Check::warn(NAME, format!("{linked}, but not on the bar"), ENABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Omarchy default, in the order the shell writes it.
    const DEFAULT: &str = r#"{
  "version": 1,
  "bar": {
    "position": "top",
    "layout": {
      "left": [
        { "id": "omarchy.menu" },
        { "id": "omarchy.workspaces" }
      ],
      "center": [],
      "centerEnd": [
        { "id": "omarchy.weather" }
      ],
      "right": [
        {
          "id": "omarchy.tray"
        },
        {
          "id": "omarchy.audio"
        }
      ]
    }
  },
  "plugins": []
}
"#;

    fn no_clones(_: &str, _: &str) -> bool {
        false
    }

    fn right(text: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(text).unwrap();
        section_of(&v, "right")
            .iter()
            .filter_map(|e| element_id(e).map(str::to_string))
            .collect()
    }

    #[test]
    fn the_widget_goes_right_after_the_tray() {
        let added = add_widget(DEFAULT, "right", &no_clones).unwrap().unwrap();
        assert_eq!(
            right(&added.text),
            ["omarchy.tray", "ferry.bar", "omarchy.audio"]
        );
        assert_eq!(
            added.placed,
            Some(Placed {
                section: "right".into(),
                index: 1,
                after: Some("omarchy.tray".into())
            })
        );
        assert!(!added.reenabled);
        // only the entry is new, drawn as the object it is, and the keys stay in the file's order
        let ctx = |l: &str| format!("  {l}");
        let add = |l: &str| format!("+ {l}");
        assert_eq!(
            change_lines(DEFAULT, &added.text, 1),
            [
                ctx("        },"),
                add("        {"),
                add("          \"id\": \"ferry.bar\""),
                add("        },"),
                ctx("        {"),
            ]
        );
        assert!(
            added
                .text
                .starts_with("{\n  \"version\": 1,\n  \"bar\": {\n    \"position\"")
        );
    }

    #[test]
    fn a_clone_of_the_tray_is_the_anchor_when_the_tray_itself_is_not_there() {
        let text = DEFAULT.replace("omarchy.tray", "my.tray");
        let clone = |id: &str, source: &str| id == "my.tray" && source == "omarchy.tray";
        let added = add_widget(&text, "right", &clone).unwrap().unwrap();
        assert_eq!(
            right(&added.text),
            ["my.tray", "ferry.bar", "omarchy.audio"]
        );
        assert_eq!(added.placed.unwrap().after.as_deref(), Some("my.tray"));
        // a stranger is not a clone: the end of the section
        let added = add_widget(&text, "right", &no_clones).unwrap().unwrap();
        assert_eq!(
            right(&added.text),
            ["my.tray", "omarchy.audio", "ferry.bar"]
        );
        assert_eq!(added.placed.unwrap().after, None);
    }

    #[test]
    fn the_other_sections_have_their_anchors_too() {
        let added = add_widget(DEFAULT, "left", &no_clones).unwrap().unwrap();
        let v: Value = serde_json::from_str(&added.text).unwrap();
        let ids: Vec<_> = section_of(&v, "left")
            .iter()
            .filter_map(element_id)
            .collect();
        assert_eq!(ids, ["omarchy.menu", "omarchy.workspaces", "ferry.bar"]);
        let added = add_widget(DEFAULT, "centerEnd", &no_clones)
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&added.text).unwrap();
        let ids: Vec<_> = section_of(&v, "centerEnd")
            .iter()
            .filter_map(element_id)
            .collect();
        assert_eq!(ids, ["omarchy.weather", "ferry.bar"]);
        // a section with no anchor, and an empty one: the end
        let added = add_widget(DEFAULT, "center", &no_clones).unwrap().unwrap();
        let v: Value = serde_json::from_str(&added.text).unwrap();
        assert_eq!(section_of(&v, "center").len(), 1);
    }

    #[test]
    fn a_section_the_file_lacks_is_not_invented() {
        assert!(matches!(
            add_widget(DEFAULT, "tray", &no_clones),
            Err(ShellJsonError::NoSection(s)) if s == "tray"
        ));
    }

    #[test]
    fn present_as_an_object_or_as_a_string_is_nothing_to_do() {
        let object = DEFAULT.replace("\"omarchy.audio\"", "\"ferry.bar\"");
        assert_eq!(add_widget(&object, "right", &no_clones).unwrap(), None);
        let string = DEFAULT.replace("{ \"id\": \"omarchy.weather\" }", "\"ferry.bar\"");
        assert_eq!(add_widget(&string, "right", &no_clones).unwrap(), None);
        // in another section than the plugin's own default: still on the bar
        let elsewhere = DEFAULT.replace(
            "{ \"id\": \"omarchy.menu\" }",
            "{ \"id\": \"ferry.bar\", \"showPeerName\": false }",
        );
        assert_eq!(add_widget(&elsewhere, "right", &no_clones).unwrap(), None);
    }

    #[test]
    fn disabled_plugins_is_cleaned_like_the_shell_does() {
        let disabled = DEFAULT.replace(
            "\"plugins\": []",
            "\"disabledPlugins\": [\"x\", \"ferry.bar\", \"y\"],\n  \"plugins\": []",
        );
        let added = add_widget(&disabled, "right", &no_clones).unwrap().unwrap();
        assert!(added.reenabled && added.placed.is_some());
        let v: Value = serde_json::from_str(&added.text).unwrap();
        assert_eq!(v["disabledPlugins"], json!(["x", "y"]));
        // on the bar but disabled: only the list changes
        let both = disabled.replace("\"omarchy.audio\"", "\"ferry.bar\"");
        let added = add_widget(&both, "right", &no_clones).unwrap().unwrap();
        assert!(added.reenabled && added.placed.is_none());
        let v: Value = serde_json::from_str(&added.text).unwrap();
        assert_eq!(v["disabledPlugins"], json!(["x", "y"]));
        // the only entry leaves an empty list in place
        let only = DEFAULT.replace(
            "\"plugins\": []",
            "\"disabledPlugins\": [\"ferry.bar\"],\n  \"plugins\": []",
        );
        let added = add_widget(&only, "right", &no_clones).unwrap().unwrap();
        let v: Value = serde_json::from_str(&added.text).unwrap();
        assert_eq!(v["disabledPlugins"], json!([]));
        assert!(
            added.text.contains("\"disabledPlugins\": []"),
            "{}",
            added.text
        );
    }

    #[test]
    fn a_file_the_shell_would_not_read_is_not_edited() {
        for (text, why) in [
            ("{ not json", "is not valid JSON"),
            ("[]", "is not a JSON object"),
            ("{\"bar\":{\"layout\":{}}}", "has no \"version\": 1"),
            (
                "{\"version\":\"1\",\"bar\":{\"layout\":{}}}",
                "has no \"version\": 1",
            ),
            ("{\"version\":1}", "has no bar.layout"),
            (
                "{\"version\":1,\"bar\":{\"layout\":[]}}",
                "has no bar.layout",
            ),
            (
                "{\"version\":1,\"bar\":{\"layout\":{\"right\":5}}}",
                "has no bar.layout.right",
            ),
        ] {
            let e = add_widget(text, "right", &no_clones)
                .unwrap_err()
                .to_string();
            assert!(e.starts_with(why), "{text}: {e}");
        }
        assert!(remove_widget("{ not json").is_err());
    }

    #[test]
    fn removal_takes_every_entry_in_every_section_and_nothing_else() {
        let added = add_widget(DEFAULT, "right", &no_clones).unwrap().unwrap();
        let (text, n) = remove_widget(&added.text).unwrap().unwrap();
        assert_eq!((text.as_str(), n), (DEFAULT, 1), "byte for byte");
        let twice = added.text.replace(
            "{ \"id\": \"omarchy.menu\" }",
            "{ \"id\": \"omarchy.menu\" },\n        \"ferry.bar\"",
        );
        let (text, n) = remove_widget(&twice).unwrap().unwrap();
        assert_eq!(n, 2);
        assert!(!text.contains("ferry.bar"), "{text}");
        assert_eq!(remove_widget(DEFAULT).unwrap(), None);
    }

    #[test]
    fn the_default_section_is_read_from_the_manifest() {
        let d = tempfile::tempdir().unwrap();
        let write = |text: &str| fs::write(d.path().join("manifest.json"), text).unwrap();
        assert_eq!(default_section(d.path()), "right", "no manifest");
        write("{ not json");
        assert_eq!(default_section(d.path()), "right");
        write(r#"{"barWidget":{"defaultSection":"left"}}"#);
        assert_eq!(default_section(d.path()), "left");
        write(r#"{"barWidget":{"defaultSection":"center"}}"#);
        assert_eq!(
            default_section(d.path()),
            "centerEnd",
            "center means beside the clock"
        );
        write(r#"{"barWidget":{"defaultSection":"sideways"}}"#);
        assert_eq!(default_section(d.path()), "right");
        write(r#"{"barWidget":{}}"#);
        assert_eq!(default_section(d.path()), "right");
    }

    #[test]
    fn the_real_manifest_asks_for_the_right_section() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin");
        assert_eq!(default_section(&manifest), "right");
    }

    #[test]
    fn a_clone_is_what_its_manifest_says() {
        let d = tempfile::tempdir().unwrap();
        let mk = |id: &str, manifest: &str| {
            fs::create_dir_all(d.path().join(id)).unwrap();
            fs::write(d.path().join(id).join("manifest.json"), manifest).unwrap();
        };
        mk("my.tray", r#"{"omarchy":{"clonedFrom":"omarchy.tray"}}"#);
        mk(
            "my.other",
            r#"{"omarchy":{"clonedFrom":"omarchy.network"}}"#,
        );
        mk("my.broken", "{ nope");
        let is_clone = is_clone_in(d.path());
        assert!(is_clone("my.tray", "omarchy.tray"));
        assert!(!is_clone("my.tray", "omarchy.network"));
        assert!(!is_clone("my.other", "omarchy.tray"));
        assert!(!is_clone("my.broken", "omarchy.tray"));
        assert!(!is_clone("my.absent", "omarchy.tray"));
        // an id that climbs out of the directory is never read
        assert!(!is_clone("../my.tray", "omarchy.tray"));
        assert!(!is_clone("", "omarchy.tray"));
    }

    #[test]
    fn plain_ids() {
        for ok in ["ferry.bar", "my.tray", "a", "A-b_c.d"] {
            assert!(is_plain_id(ok), "{ok}");
        }
        for bad in ["", ".hidden", "-x", "a/b", "a..b", "a b", "../x"] {
            assert!(!is_plain_id(bad), "{bad}");
        }
    }

    #[test]
    fn the_change_is_shown_with_two_lines_of_context() {
        let old = "a\nb\nc\nd\ne\nf\n";
        let new = "a\nb\nc\nX\nY\nd\ne\nf\n";
        assert_eq!(
            change_lines(old, new, 2),
            ["  b", "  c", "+ X", "+ Y", "  d", "  e"]
        );
        assert_eq!(change_lines("a\nb\n", "a\nc\n", 1), ["  a", "- b", "+ c"]);
        assert!(change_lines("same\n", "same\n", 2).is_empty());
        assert_eq!(change_lines("", "x\n", 2), ["+ x"]);
        assert_eq!(change_lines("x\n", "", 2), ["- x"]);
    }

    #[test]
    fn a_block_is_drawn_where_it_starts_at_the_shallowest_indent() {
        // the same insertion, found one line late by a plain prefix/suffix comparison
        let old = "[\n  {\n    \"id\": \"a\"\n  },\n  {\n    \"id\": \"b\"\n  }\n]\n";
        let new = "[\n  {\n    \"id\": \"a\"\n  },\n  {\n    \"id\": \"x\"\n  },\n  {\n    \"id\": \"b\"\n  }\n]\n";
        assert_eq!(
            change_lines(old, new, 1),
            ["    },", "+   {", "+     \"id\": \"x\"", "+   },", "    {"]
        );
        // and a removal is drawn the same way
        assert_eq!(
            change_lines(new, old, 1),
            ["    },", "-   {", "-     \"id\": \"x\"", "-   },", "    {"]
        );
        // a change that is both: as it is
        assert_eq!(
            change_lines("a\nb\nc\n", "a\nX\nc\n", 1),
            ["  a", "- b", "+ X", "  c"]
        );
    }

    #[test]
    fn the_link_is_ours_only_when_it_points_at_the_package() {
        let d = tempfile::tempdir().unwrap();
        let source = d.path().join("pkg/plugin");
        fs::create_dir_all(&source).unwrap();
        let plugins = d.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        let link = plugins.join("ferry.bar");
        assert!(matches!(link_state(&link, &source).unwrap(), Link::Absent));

        symlink(&source, &link).unwrap();
        assert!(matches!(link_state(&link, &source).unwrap(), Link::Ours));
        fs::remove_file(&link).unwrap();

        // the same place by another name
        symlink("../pkg/plugin", &link).unwrap();
        assert!(matches!(link_state(&link, &source).unwrap(), Link::Ours));
        fs::remove_file(&link).unwrap();

        // a link that dangles, to where the package would put it, is still setup's
        let gone = d.path().join("gone/plugin");
        symlink(&gone, &link).unwrap();
        assert!(matches!(link_state(&link, &gone).unwrap(), Link::Ours));
        assert!(
            matches!(link_state(&link, &source).unwrap(), Link::Foreign(w) if w.starts_with("linked to "))
        );
        fs::remove_file(&link).unwrap();

        fs::create_dir(&link).unwrap();
        assert!(
            matches!(link_state(&link, &source).unwrap(), Link::Foreign(w) if w == "a directory")
        );
        fs::remove_dir(&link).unwrap();
        fs::write(&link, "x").unwrap();
        assert!(matches!(link_state(&link, &source).unwrap(), Link::Foreign(w) if w == "a file"));
    }

    use proptest::prelude::*;

    fn entry() -> impl Strategy<Value = Value> {
        let id = prop_oneof![
            Just("omarchy.tray".to_string()),
            Just("my.tray".to_string()),
            "[a-z]{1,6}\\.[a-z]{1,6}",
        ];
        (id, any::<bool>(), any::<bool>()).prop_map(|(id, bare, extra)| {
            if bare {
                Value::String(id)
            } else if extra {
                json!({ "id": id, "size": 3, "list": ["]", {"k": "}"}] })
            } else {
                json!({ "id": id })
            }
        })
    }

    fn layouts() -> impl Strategy<Value = Value> {
        proptest::collection::vec(entry(), 0..6).prop_map(|right| {
            json!({
                "version": 1,
                "cloneSourceRestores": [],
                "bar": { "id": "my.bar", "layout": { "left": [], "centerEnd": ["omarchy.weather"], "right": right } },
                "disabledPlugins": ["a", "ferry.bar"],
                "plugins": [],
            })
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Whatever the file looks like, adding the widget gives a file the shell reads with the
        /// widget in it, and taking it away gives the same file back, byte for byte, apart from the
        /// `disabledPlugins` entry the add took out.
        #[test]
        fn add_then_remove(value in layouts(), pretty in any::<bool>()) {
            let text = if pretty { serde_json::to_string_pretty(&value).unwrap() + "\n" } else { serde_json::to_string(&value).unwrap() };
            let clone = |id: &str, source: &str| id == "my.tray" && source == "omarchy.tray";
            for section in ["right", "centerEnd", "left"] {
                let added = add_widget(&text, section, &clone).unwrap().unwrap();
                prop_assert!(added.reenabled);
                prop_assert!(added.placed.is_some());
                let v: Value = serde_json::from_str(&added.text).unwrap();
                prop_assert_eq!(widget_at(&v, section).len(), 1);
                prop_assert!(disabled_at(&v).is_empty());
                let (back, n) = remove_widget(&added.text).unwrap().unwrap();
                prop_assert_eq!(n, 1);
                let mut without = value.clone();
                without["disabledPlugins"] = json!(["a"]);
                let want = if pretty { serde_json::to_string_pretty(&without).unwrap() + "\n" } else { serde_json::to_string(&without).unwrap() };
                prop_assert_eq!(back, want, "section {}", section);
            }
        }

        /// The user's file is never read in a way that can panic.
        #[test]
        fn nothing_panics(text in "\\PC{0,200}") {
            let _ = add_widget(&text, "right", &no_clones);
            let _ = remove_widget(&text);
            let _ = change_lines(&text, &text.replace('a', "b"), 2);
        }
    }
}
