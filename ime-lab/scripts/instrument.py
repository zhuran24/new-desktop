#!/usr/bin/env python3
"""Add observation-only hooks to private gpui-base copies; preserve original logic."""
from pathlib import Path
import difflib

ROOT = Path(__file__).resolve().parents[1]
OBSERVER = '''//! ime-lab observation hooks. No input decisions are made here.
use std::sync::OnceLock;
use serde_json::Value;
type Observer = Box<dyn Fn(Value) + Send + Sync>;
static OBSERVER: OnceLock<Observer> = OnceLock::new();
pub fn install(observer: impl Fn(Value) + Send + Sync + 'static) {
    assert!(OBSERVER.set(Box::new(observer)).is_ok(), "observer already installed");
}
pub(crate) fn emit(event: Value) {
    if let Some(observer) = OBSERVER.get() { observer(event); }
}
'''

SNAPSHOT = '''    /// Lab-only observation; ranges are UTF-16 code units.
    #[doc(hidden)]
    pub fn ime_lab_snapshot(&self) -> serde_json::Value {
        let marked = self.ime_marked_range.map(|r| r.start..r.end);
        serde_json::json!({
            "has_preedit": marked.is_some(),
            "preedit_text": marked.as_ref().map(|r| self.text.slice(r.clone()).to_string()),
            "marked_range_utf16": marked.as_ref().map(|r| self.range_to_utf16(r)),
            "selection_utf16": self.range_to_utf16(&self.selected_range()),
        })
    }

'''

def wrap(text, name, before, after):
    start = text.index('    fn ' + name + '(')
    body = text.index(' {\n', start) + 2
    end = text.index('\n    }\n', body)
    original = text[body:end]
    return text[:body] + '\n' + before + '\n        let ime_lab_result = (|| {' + original + '\n        })();\n' + after + '\n        ime_lab_result' + text[end:]

for variant, base in [('v070', ROOT/'vendor/base-v070'), ('main', ROOT/'vendor/main-upstream/crates/base')]:
    files = ['src/lib.rs', 'src/input/base/state.rs', 'src/ime_lab_observer.rs']
    original = {p: (base/p).read_text() if (base/p).exists() else '' for p in files}
    assert 'ime_lab_observer' not in original['src/lib.rs'], 'Already instrumented'
    (base/'src/lib.rs').write_text(original['src/lib.rs'] + '\n#[doc(hidden)]\npub mod ime_lab_observer;\n')
    (base/'src/ime_lab_observer.rs').write_text(OBSERVER)
    state = original['src/input/base/state.rs']
    anchor = '    pub fn set_clean_on_escape('
    state = state.replace(anchor, SNAPSHOT + anchor, 1)
    for name, event, args in [
        ('replace_text_in_range', 'text_commit', '"text": new_text, "replacement_range_utf16": range_utf16,'),
        ('replace_and_mark_text_in_range', 'preedit_update', '"text": new_text, "replacement_range_utf16": range_utf16, "requested_selection_utf16": new_selected_range_utf16,'),
        ('unmark_text', 'unmark', ''),
    ]:
        before = '        let ime_lab_before = self.ime_lab_snapshot();'
        after = f'''        crate::ime_lab_observer::emit(serde_json::json!({{
            "event": "{event}", "source": "EntityInputHandler", {args}
            "before": ime_lab_before, "after": self.ime_lab_snapshot(),
            "value_after": self.value().to_string(),
        }}));'''
        state = wrap(state, name, before, after)
    before = '''        let ime_lab_range = range_utf16.clone();
        let ime_lab_before = self.ime_lab_snapshot();'''
    after = '''        crate::ime_lab_observer::emit(serde_json::json!({
            "event": "cursor_bounds", "source": "EntityInputHandler::bounds_for_range",
            "range_utf16": ime_lab_range, "state": ime_lab_before,
            "input_bounds": {"x": f32::from(bounds.origin.x), "y": f32::from(bounds.origin.y),
                "width": f32::from(bounds.size.width), "height": f32::from(bounds.size.height)},
            "rect": ime_lab_result.map(|r| serde_json::json!({"x": f32::from(r.origin.x),
                "y": f32::from(r.origin.y), "width": f32::from(r.size.width), "height": f32::from(r.size.height)})),
        }));'''
    state = wrap(state, 'bounds_for_range', before, after)
    # Record the action before upstream handles it; deliberately no guard here.
    sig = '    pub(super) fn enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {\n'
    assert sig in state
    state = state.replace(sig, sig + '''        crate::ime_lab_observer::emit(serde_json::json!({
            "event": "kit_enter", "secondary": action.secondary, "shift": action.shift,
            "state": self.ime_lab_snapshot(),
        }));
''', 1)
    (base/'src/input/base/state.rs').write_text(state)
    patch = ''.join(''.join(difflib.unified_diff(original[p].splitlines(True), (base/p).read_text().splitlines(True),
        fromfile='a/'+p if original[p] else '/dev/null', tofile='b/'+p)) for p in files)
    (ROOT/f'patches/observe-{variant}.patch').write_text(patch)
