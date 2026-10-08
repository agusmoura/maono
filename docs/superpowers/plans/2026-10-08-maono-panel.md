# maono Omarchy panel (plan 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the upstream single-file bar widget with an Omarchy shell plugin `io.github.agusmoura.maono`. It has a bar icon and a 420 px popup with six tabs (Voz, EQ, Filtro, Luz, Perfiles, Ajustes), a hero with mute, live meter, the "Live" monitor and a profile selector, full keyboard navigation, Spanish/English strings, and nothing hardcoded. Ranges, options, presets and colours all come from `maono serve`'s schema.

**Architecture:**
- **Service:** one shared `Service.qml` (kind `service`) owns a `maono serve` child process. It sends JSONL requests on stdin, reduces JSONL events from stdout into a bindable `status`, and keeps per-key optimistic edits until their ack.
- **Bar widget:** `BarWidget.qml` (one per monitor) shows the icon, handles clicks and the wheel, and hosts `Panel.qml` through a Loader.
- **Panel:** `Panel.qml` holds the hero, the tab strip and a cursor model. Each tab is its own file and receives the panel as `panel`.
- **Logic:** everything that is logic lives in `Model.js` (Node-tested); all text lives in `Strings.js` (Node-tested for es/en parity).

**Tech Stack:** Quickshell QML (Omarchy shell 4.x, `qs.Ui` / `qs.Commons`), Quickshell.Io `Process`/`SplitParser`/`IpcHandler`, Quickshell.Services.Pipewire `PwNodePeakMonitor`, plain ES5-style JS for `Model.js`/`Strings.js`, Node 26 (`node --test`), Rust for the installer (`src/shell.rs`).

**Spec:** `docs/superpowers/specs/2026-10-07-omarchy-panel-design.md` (rev 2 + Live amendment).

**Backend contract (as built by plan 1, verify in `src/engine.rs`):**
- Commands (field `cmd`, request id in `id`):
  - `status`, `refresh`, `set {changes}`, `filter.set {changes}`
  - `source.default {which: clean|raw}`
  - `profile.list`, `profile.apply {profile}`, `profile.save {name, groups?, overwrite?}`, `profile.rename {profile, name}`, `profile.duplicate {profile}`, `profile.delete {profile}`
  - `eq.preset.save {name}`, `eq.preset.delete {preset}`
  - `light.custom {op: add|update|delete, index?, hsv?}`
  - `config.set {changes}`, `recover`
  - `monitor.set {on, source?, force?}`: refused with `error: "not-headphones"` unless forced; refused in one-shot mode
- Events (field `ev`):
  - `hello {protocol: 1, schema: {device, filter}, presets, profiles, config, warnings}`
  - `state {device, model, mic, filter, filterApplied, deps, activeProfile, dirty, partial, defaultSource, cleanSource, monitor: {on, source, headphones}}`
  - `changed {key, value, source}`
  - `ack {id, ok, ...}`: `effective` on set/profile.apply/recover, `filter` on filter.set, `error`/`message`/`details`/`failed`/`warnings` on failure
  - `profiles {profiles}`, `presets {presets}`
  - `reapplied {profile, result, recoverable}`
  - `error {code, message}`, with code `device|identity|filter|monitor`
  - `device` is one of `connected|disconnected|permission|unsupported`. An unknown value is `null`.

**Repo:** `~/dev/maono`, branch `omarchy-panel`, continuing after plan 1 (HEAD `152c426` or later). The plugin source lives in `shell/` (flat), and `maono shell install` copies it.

## Global Constraints

- Plugin id `io.github.agusmoura.maono`. The manifest has `kinds: ["service", "bar-widget"]` and `entryPoints: {"service": "Service.qml", "barWidget": "BarWidget.qml"}`. Never add `"panel"` to kinds.
- The plugin folder is flat, real files, no symlinks. Nothing at runtime writes inside it (the shell hot-reloads on any write there).
- Imports: `import QtQuick.Controls` comes **before** `import qs.Ui`. Any widget or panel gets the service through `bar.shell.serviceFor("io.github.agusmoura.maono")`, null-guarded, with the 200 ms re-poll while it is null.
- No literal colours except the light swatches, which take their hex from the schema. Use `Style.*`, `Color.*`, `bar.foreground`/`bar.urgent`/`bar.fontFamily`, and `Qt.darker(fg, 1.4)` for dim text.
- No user-facing string literals in QML: every label goes through `tr(key)` → `Strings.js` (`es` and `en`, same keys). Ranges, steps, units and enum options come from the schema.
- `Model.js` and `Strings.js` contain only top-level `var` and `function` (Node `vm` harness): no `.pragma`, no `const`/`let`/`class` at top level, no `module.exports`.
- Panel width is `Style.space(420)` and height is capped at `Style.space(640)`, with a ScrollView. Sliders ignore the mouse wheel (QuietSlider). Slider writes are debounced to 120 ms.
- **Keyboard:**
  - `j`/`k` move between rows; `h`/`l` adjust; Enter/Space activate; `x` deletes; Esc closes; Tab goes to the neighbouring panel.
  - Digits `1`–`6` pick a tab; `m` toggles mute; `v` toggles Live; `r` reapplies the profile.
  - An open text field blocks the catcher.
- Optimistic UI: per key, with a request id. An ack settles only its own keys. The values in an ack's `effective`/`filter` are applied before the pending entry is dropped, so nothing flickers.
- Real hardware (plugin install, `maono serve` started by the Service, PipeWire) happens only in Task 15, after Agus says yes, together with plan 1's Task 14.
- Verification commands:
  - `node --test shell/tests/`
  - QML syntax: `for f in shell/*.qml; do /usr/lib/qt6/bin/qmllint "$f" 2>&1 | grep '\[syntax\]' && echo "SYNTAX ERROR in $f"; done`. It must print nothing. Unresolved `qs.*` import warnings are expected and fine.
  - Rust: `mise exec rust@stable -- cargo test --lib`

## Deliberate simplifications vs the spec

- Strings live in `Strings.js` (one data file holding both languages) instead of `i18n/{es,en}.json` loaded through FileView. It loads synchronously, Node can test it, and it stays text-only data.
- Mouse hover moves the shared cursor but never scrolls; only the keyboard scrolls the cursor into view.
- The hero's mute is a key (`m`) and a switch in the hero; Live and its source are buttons in the hero and the `v` key. Only the profile chips and the tab strip are keyboard rows at the top.
- `maono shell install` prints the migration commands (disable the upstream `maono` widget, enable the new id at the same index) instead of running them; Task 15 runs them with Agus present.

## Review Focus

1. The backend is not running or the binary is missing: the panel shows the "no corre maono serve" banner, every control is disabled, the icon is dimmed, and nothing throws. Test: Task 1 `banner_for_every_backend_state`.
2. An ack arrives for an older request after a newer edit of the same key: the newer optimistic value stays until its own ack. Test: Task 1 `older_ack_does_not_clear_newer_edit`.
3. A refused Live (`not-headphones`) shows the force prompt; any other failure shows the error, never a silent no-op. Test: Task 1 `live_refusal_is_classified`.
4. The schema is missing a field (an older backend): rows fall back to the field's safe defaults instead of NaN/undefined ranges. Test: Task 1 `field_fallbacks`.
5. Spanish and English tables drift apart: a missing key would show the raw key. Test: Task 3 `es_and_en_have_the_same_keys` plus `every_key_used_in_qml_exists`.

---

### Task 1: `Model.js` core (protocol reducer, optimistic edits, schema helpers, i18n)

**Files:**
- Create: `shell/Model.js`
- Create: `shell/tests/model.test.js`

**Interfaces (all top-level functions in Model.js):**
- `PROTOCOL = 1`, `clamp(v, lo, hi)`, `emptyStatus() -> {ready, schema, presets, profiles, config, st, warnings, notice, running}`
- `applyEvent(status, ev) -> status`: hello/state/changed/profiles/presets/reapplied/error, never mutating
- `applyAck(status, ack) -> status`: merges `ack.effective` into `st.mic` and `ack.filter` into `st.filter`
- `withNotice(status, notice)`, `withConfig(status, changes)`, `backendExited(status, exitCode)`
- `withPending(pending, changes, id)`, `settlePending(pending, id)`, `effective(pending, map, key)`, `effectiveMap(pending, map)`, `getPath(obj, "a.b.0.c")`, `overlayFilter(filter, pendingFilter)`
- `field(schema, key)`, `filterField(schema, path)`, `fieldMin/fieldMax/fieldStep(schema, key, fallback)`, `filterMin/filterMax/filterStep(schema, path, fallback)`, `enumOptions(schema, key, tr)`, `groupKeys(schema, group, prefix, exclude)`, `fieldLabel(tr, key)`
- `nrChoice(mic)`, `nrChanges(choice)`, `nrOptions(schema, tr)`
- `nextProfileId(profiles, active)`, `profileName(profiles, id)`
- `langFor(setting, localeName, available)`, `t(strings, lang, key, params)`
- `banner(hasService, status) -> string key or ""`, `liveRefusal(ack) -> "force" | "error" | ""`, `ackText(ack)`
- `formatValue(v, unit)`, `formatHz(f)`, `freqToPos(f, lo, hi)`, `posToFreq(p, lo, hi)`, `hsvToHex(hsv)`, `glyph(name)`, `barTooltip(tr, st, profiles)`
- `prefSpec(manifest, key)`, `prefDefault(manifest, key, fallback)` (UI preferences declared in manifest.json)

- [ ] **Step 1: Write the failing tests**

`shell/tests/model.test.js`:

```js
const test = require("node:test")
const assert = require("node:assert/strict")
const fs = require("node:fs")
const path = require("node:path")
const vm = require("node:vm")

const M = {}
vm.createContext(M)
vm.runInContext(fs.readFileSync(path.join(__dirname, "..", "Model.js"), "utf8"), M, { filename: "Model.js" })

const schema = {
  device: {
    fields: [
      { key: "mic.gain", type: "int", min: 0, max: 20, step: 1 },
      { key: "mic.nr.on", type: "bool" },
      { key: "mic.nr.level", type: "enum", options: [{ value: 0, label: "nr.low" }, { value: 1, label: "nr.mid" }, { value: 2, label: "nr.high" }] },
      { key: "dsp.comp.on", type: "bool", group: "dsp" },
      { key: "dsp.eq.0.freq", type: "int", group: "dsp", min: 20, max: 20000 },
    ],
    light: { presets: [{ value: 0, name: "white", hex: "#FFFFFF" }] },
  },
  filter: { fields: [{ key: "hpf.freq", type: "num", min: 10, max: 400, step: 1, unit: "Hz" }, { key: "eq.bands.*.gain", type: "num", min: -24, max: 24, step: 0.1, unit: "dB" }] },
}

test("hello and state build a ready status", () => {
  let s = M.emptyStatus()
  s = M.applyEvent(s, { ev: "hello", protocol: 1, schema, presets: [{ id: "original" }], profiles: [{ id: "llamada", name: "Llamada" }], config: { applyOnReconnect: true }, warnings: ["w"] })
  assert.equal(s.ready, true)
  assert.equal(s.profiles[0].id, "llamada")
  s = M.applyEvent(s, { ev: "state", device: "connected", mic: { "mic.gain": 20 }, filter: { hpf: { on: true, freq: 90 } } })
  assert.equal(s.st.mic["mic.gain"], 20)
})

test("wrong protocol is not ready and says why", () => {
  const s = M.applyEvent(M.emptyStatus(), { ev: "hello", protocol: 9, schema })
  assert.equal(s.ready, false)
  assert.equal(s.notice.key, "notice.protocol")
})

test("changed patches mic without mutating the previous status", () => {
  const a = M.applyEvent(M.emptyStatus(), { ev: "state", device: "connected", mic: { "mic.mute": false } })
  const b = M.applyEvent(a, { ev: "changed", key: "mic.mute", value: true, source: "button" })
  assert.equal(a.st.mic["mic.mute"], false)
  assert.equal(b.st.mic["mic.mute"], true)
})

test("reapplied and error events become notices", () => {
  const s = M.applyEvent(M.emptyStatus(), { ev: "reapplied", profile: "llamada", recoverable: true })
  assert.equal(s.notice.kind, "recover")
  const e = M.applyEvent(M.emptyStatus(), { ev: "error", code: "filter", message: "boom" })
  assert.deepEqual([e.notice.kind, e.notice.key, e.notice.text], ["error", "error.filter", "boom"])
})

test("ack effective values land before the pending entry is dropped", () => {
  let s = M.applyEvent(M.emptyStatus(), { ev: "state", device: "connected", mic: { "mic.gain": 20 }, filter: { rnnoise: { vad: 85 } } })
  s = M.applyAck(s, { ev: "ack", id: 3, ok: true, effective: { "mic.gain": 12 } })
  assert.equal(s.st.mic["mic.gain"], 12)
  s = M.applyAck(s, { ev: "ack", id: 4, ok: true, filter: { rnnoise: { vad: 60 } } })
  assert.equal(s.st.filter.rnnoise.vad, 60)
})

test("older_ack_does_not_clear_newer_edit", () => {
  let p = M.withPending({}, { "mic.gain": 10 }, 1)
  p = M.withPending(p, { "mic.gain": 12 }, 2)
  p = M.settlePending(p, 1)
  assert.equal(M.effective(p, { "mic.gain": 20 }, "mic.gain"), 12)
  p = M.settlePending(p, 2)
  assert.equal(M.effective(p, { "mic.gain": 20 }, "mic.gain"), 20)
  assert.deepEqual(M.effectiveMap(M.withPending({}, { a: 1 }, 5), { a: 0, b: 2 }), { a: 1, b: 2 })
})

test("paths and filter overlay", () => {
  const f = { eq: { bands: [{ gain: 0 }, { gain: 1 }] }, hpf: { freq: 90 } }
  assert.equal(M.getPath(f, "eq.bands.1.gain"), 1)
  assert.equal(M.getPath(null, "a.b"), undefined)
  const o = M.overlayFilter(f, M.withPending({}, { "eq.bands.0.gain": 6, "hpf.freq": 120 }, 1))
  assert.equal(o.eq.bands[0].gain, 6)
  assert.equal(o.hpf.freq, 120)
  assert.equal(f.eq.bands[0].gain, 0, "the confirmed filter is untouched")
})

test("field_fallbacks", () => {
  assert.equal(M.fieldMax(schema, "mic.gain", 99), 20)
  assert.equal(M.fieldMax(schema, "nope", 99), 99)
  assert.equal(M.fieldMax(null, "mic.gain", 7), 7)
  assert.equal(M.filterMax(schema, "eq.bands.3.gain", 0), 24)
  assert.equal(M.filterStep(schema, "eq.bands.3.gain", 1), 0.1)
  assert.equal(M.filterMin(schema, "missing.path", -5), -5)
})

test("enum options, groups and labels", () => {
  const tr = (k) => "T:" + k
  assert.deepEqual(M.enumOptions(schema, "mic.nr.level", tr).map((o) => o.label), ["T:nr.low", "T:nr.mid", "T:nr.high"])
  assert.deepEqual(M.groupKeys(schema, "dsp", "", "dsp.eq."), ["dsp.comp.on"])
  assert.deepEqual(M.groupKeys(schema, "dsp", "dsp.eq.", ""), ["dsp.eq.0.freq"])
  assert.equal(M.fieldLabel((k) => (k === "field.dsp.comp.on" ? "Compresor" : k), "dsp.comp.on"), "Compresor")
  assert.equal(M.fieldLabel((k) => k, "dsp.eq.3.gain"), "eq.internal 4 · eq.gain")
})

test("noise reduction is one four-way choice", () => {
  assert.equal(M.nrChoice({ "mic.nr.on": false, "mic.nr.level": 2 }), "off")
  assert.equal(M.nrChoice({ "mic.nr.on": true, "mic.nr.level": 2 }), "lvl:2")
  assert.equal(M.nrChoice({ "mic.nr.on": null }), null)
  assert.deepEqual(M.nrChanges("off"), { "mic.nr.on": false })
  assert.deepEqual(M.nrChanges("lvl:1"), { "mic.nr.on": true, "mic.nr.level": 1 })
  assert.deepEqual(M.nrOptions(schema, (k) => k).map((o) => o.value), ["off", "lvl:0", "lvl:1", "lvl:2"])
})

test("profiles cycle and name lookup", () => {
  const ps = [{ id: "a", name: "A" }, { id: "b", name: "B" }]
  assert.equal(M.nextProfileId(ps, "a"), "b")
  assert.equal(M.nextProfileId(ps, "b"), "a")
  assert.equal(M.nextProfileId(ps, null), "a")
  assert.equal(M.nextProfileId([], "a"), null)
  assert.equal(M.profileName(ps, "b"), "B")
  assert.equal(M.profileName(ps, "zz"), "zz")
})

test("language and translation", () => {
  const strings = { es: { hi: "hola {name}" }, en: { hi: "hi {name}", only: "en only" } }
  assert.equal(M.langFor("auto", "es_AR", ["es", "en"]), "es")
  assert.equal(M.langFor("auto", "fr_FR", ["es", "en"]), "en")
  assert.equal(M.langFor("en", "es_AR", ["es", "en"]), "en")
  assert.equal(M.t(strings, "es", "hi", { name: "Agus" }), "hola Agus")
  assert.equal(M.t(strings, "es", "only"), "en only")
  assert.equal(M.t(strings, "es", "missing.key"), "missing.key")
})

test("banner_for_every_backend_state", () => {
  const base = M.emptyStatus()
  assert.equal(M.banner(false, base), "banner.noService")
  assert.equal(M.banner(true, Object.assign({}, base, { running: false })), "banner.noBackend")
  assert.equal(M.banner(true, Object.assign({}, base, { running: true })), "banner.connecting")
  const st = (device, extra) => Object.assign({}, base, { running: true, ready: true, st: Object.assign({ device, filterApplied: true }, extra || {}) })
  assert.equal(M.banner(true, st("disconnected")), "banner.disconnected")
  assert.equal(M.banner(true, st("permission")), "banner.permission")
  assert.equal(M.banner(true, st("unsupported")), "banner.unsupported")
  assert.equal(M.banner(true, st("connected", { filterApplied: false })), "banner.filterDown")
  assert.equal(M.banner(true, st("connected")), "")
})

test("live_refusal_is_classified", () => {
  assert.equal(M.liveRefusal({ ok: true }), "")
  assert.equal(M.liveRefusal({ ok: false, error: "not-headphones" }), "force")
  assert.equal(M.liveRefusal({ ok: false, error: "the mic is not visible in PipeWire" }), "error")
  assert.equal(M.ackText({ ok: false, error: "invalid", details: ["a", "b"] }), "invalid: a; b")
  assert.equal(M.ackText({ ok: false, error: "x", message: "longer" }), "longer")
})

test("backend exit is a notice and not ready", () => {
  const s = M.backendExited(Object.assign(M.emptyStatus(), { ready: true, running: true }), 2)
  assert.equal(s.ready, false)
  assert.equal(s.running, false)
  assert.equal(s.notice.key, "error.otherServe")
  assert.equal(M.backendExited(M.emptyStatus(), 1).notice.key, "error.serveExited")
})

test("config and notice helpers", () => {
  const s = M.withConfig(Object.assign(M.emptyStatus(), { config: { a: 1 } }), { applyOnReconnect: false })
  assert.deepEqual(s.config, { a: 1, applyOnReconnect: false })
  assert.equal(M.withNotice(s, null).notice, null)
})

test("formatting and slider mappings", () => {
  assert.equal(M.formatHz(90), "90 Hz")
  assert.equal(M.formatHz(16822.2), "16.8 kHz")
  assert.equal(M.formatValue(3.25, "dB"), "3.3 dB")
  assert.equal(M.formatValue(20, ""), "20")
  assert.equal(M.formatValue(null, "dB"), "—")
  assert.ok(Math.abs(M.posToFreq(M.freqToPos(1000, 20, 20000), 20, 20000) - 1000) < 15)
  assert.equal(M.freqToPos(20, 20, 20000), 0)
  assert.equal(M.freqToPos(20000, 20, 20000), 1000)
  assert.equal(M.hsvToHex([0, 1, 1]), "#ff0000")
  assert.equal(M.hsvToHex([120, 1, 0.5]), "#008000")
  assert.equal(M.hsvToHex(null), "#000000")
  assert.equal(typeof M.glyph("mic"), "string")
})

test("ui preferences come from the manifest schema", () => {
  const manifest = { barWidget: { schema: [{ key: "wheelStep", type: "integer", min: 1, max: 5, defaultValue: 1 }] } }
  assert.equal(M.prefSpec(manifest, "wheelStep").max, 5)
  assert.equal(M.prefDefault(manifest, "wheelStep", 9), 1)
  assert.equal(M.prefDefault(manifest, "nope", 9), 9)
  assert.equal(M.prefDefault(null, "wheelStep", 9), 9)
})

test("bar tooltip", () => {
  const tr = (k, p) => k + (p ? JSON.stringify(p) : "")
  const st = { device: "connected", mic: { "mic.mute": false, "info.battery": 70, "mic.gain": 18 }, activeProfile: "a" }
  const text = M.barTooltip(tr, st, [{ id: "a", name: "Llamada" }])
  assert.ok(text.includes("tooltip.live") && text.includes("70") && text.includes("Llamada"), text)
  assert.ok(M.barTooltip(tr, null, []).includes("tooltip.offline"))
})
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `node --test shell/tests/`
Expected: FAIL. The `vm.runInContext` call throws `ENOENT ... Model.js`.

- [ ] **Step 3: Implement `shell/Model.js`**

```js
// No QML imports: this file is intentionally executable in the Node test harness.
// Only top-level var/function declarations (see shell/tests/model.test.js).

var PROTOCOL = 1

function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)) }

function copy(o) { var out = {}; for (var k in o) out[k] = o[k]; return out }

function emptyStatus() {
  return { ready: false, running: false, schema: null, presets: [], profiles: [], config: {}, st: null, warnings: [], notice: null }
}

// One serve event -> a new status object (the input is never mutated).
function applyEvent(status, ev) {
  var s = copy(status)
  switch (ev.ev) {
  case "hello":
    s.running = true
    s.ready = ev.protocol === PROTOCOL
    s.schema = ev.schema || null
    s.presets = ev.presets || []
    s.profiles = ev.profiles || []
    s.config = ev.config || {}
    s.warnings = ev.warnings || []
    s.notice = s.ready ? null : { kind: "error", key: "notice.protocol", params: { got: ev.protocol } }
    break
  case "state":
    s.running = true
    s.st = ev
    break
  case "changed":
    if (s.st) {
      var mic = copy(s.st.mic || {})
      mic[ev.key] = ev.value
      var st = copy(s.st)
      st.mic = mic
      s.st = st
    }
    break
  case "profiles": s.profiles = ev.profiles || []; break
  case "presets": s.presets = ev.presets || []; break
  case "reapplied":
    s.notice = { kind: ev.recoverable ? "recover" : "info", key: ev.recoverable ? "notice.reappliedRecover" : "notice.reapplied", params: { profile: ev.profile } }
    break
  case "error":
    s.notice = { kind: "error", key: "error." + ev.code, text: ev.message || "" }
    break
  }
  return s
}

// Values an ack confirms, applied before its pending entries are dropped (no flicker).
function applyAck(status, ack) {
  if (!status.st) return status
  var s = copy(status)
  var st = copy(s.st)
  if (ack.effective && typeof ack.effective === "object") {
    var mic = copy(st.mic || {})
    for (var k in ack.effective) mic[k] = ack.effective[k]
    st.mic = mic
  }
  if (ack.filter && typeof ack.filter === "object") st.filter = ack.filter
  s.st = st
  return s
}

function withNotice(status, notice) { var s = copy(status); s.notice = notice; return s }

function withConfig(status, changes) {
  var s = copy(status)
  var c = copy(s.config || {})
  for (var k in changes) c[k] = changes[k]
  s.config = c
  return s
}

function backendExited(status, exitCode) {
  var s = copy(status)
  s.ready = false
  s.running = false
  s.notice = { kind: "error", key: exitCode === 2 ? "error.otherServe" : "error.serveExited", params: { code: exitCode } }
  return s
}

// pending: { key: { value, id } } — optimistic edits waiting for the ack with that id.
function withPending(pending, changes, id) {
  var p = copy(pending)
  for (var k in changes) p[k] = { value: changes[k], id: id }
  return p
}

function settlePending(pending, id) {
  var p = {}
  for (var k in pending) if (pending[k].id !== id) p[k] = pending[k]
  return p
}

function effective(pending, map, key) {
  if (Object.prototype.hasOwnProperty.call(pending, key)) return pending[key].value
  return map ? map[key] : undefined
}

function effectiveMap(pending, map) {
  var out = copy(map || {})
  for (var k in pending) out[k] = pending[k].value
  return out
}

function getPath(obj, path) {
  var parts = String(path).split(".")
  var v = obj
  for (var i = 0; i < parts.length; i++) {
    if (v === null || v === undefined) return undefined
    v = v[parts[i]]
  }
  return v
}

function overlayFilter(filter, pendingFilter) {
  if (!filter) return null
  var out = JSON.parse(JSON.stringify(filter))
  for (var path in pendingFilter) {
    var parts = path.split(".")
    var o = out
    for (var i = 0; i < parts.length - 1 && o; i++) o = o[parts[i]]
    if (o) o[parts[parts.length - 1]] = pendingFilter[path].value
  }
  return out
}

function field(schema, key) {
  var list = schema && schema.device ? (schema.device.fields || []).concat(schema.device.readonly || []) : []
  for (var i = 0; i < list.length; i++) if (list[i].key === key) return list[i]
  return null
}

function filterField(schema, path) {
  var list = schema && schema.filter ? schema.filter.fields || [] : []
  var generic = String(path).replace(/\.\d+\./, ".*.")
  for (var i = 0; i < list.length; i++) if (list[i].key === path || list[i].key === generic) return list[i]
  return null
}

function pick(f, prop, fallback) { return f && f[prop] !== undefined && f[prop] !== null ? f[prop] : fallback }
function fieldMin(schema, key, fallback) { return pick(field(schema, key), "min", fallback) }
function fieldMax(schema, key, fallback) { return pick(field(schema, key), "max", fallback) }
function fieldStep(schema, key, fallback) { return pick(field(schema, key), "step", fallback) }
function filterMin(schema, path, fallback) { return pick(filterField(schema, path), "min", fallback) }
function filterMax(schema, path, fallback) { return pick(filterField(schema, path), "max", fallback) }
function filterStep(schema, path, fallback) { return pick(filterField(schema, path), "step", fallback) }
function filterUnit(schema, path) { return pick(filterField(schema, path), "unit", "") }
function fieldUnit(schema, key) { return pick(field(schema, key), "unit", "") }

function enumOptions(schema, key, tr) {
  var f = field(schema, key)
  if (!f || !f.options) return []
  return f.options.map(function (o) { return { value: o.value, label: tr(o.label) } })
}

// Keys of a descriptor group, optionally restricted to a prefix and/or excluding one.
function groupKeys(schema, group, prefix, exclude) {
  var list = schema && schema.device ? schema.device.fields || [] : []
  var out = []
  for (var i = 0; i < list.length; i++) {
    var k = list[i].key
    if (list[i].group !== group) continue
    if (prefix && k.indexOf(prefix) !== 0) continue
    if (exclude && k.indexOf(exclude) === 0) continue
    out.push(k)
  }
  return out
}

// "field.<key>" when translated; internal EQ slots get "<eq.internal> N · <eq.<name>>".
function fieldLabel(tr, key) {
  var direct = tr("field." + key)
  if (direct !== "field." + key) return direct
  var m = /^dsp\.eq\.(\d+)\.(\w+)$/.exec(key)
  if (m) return tr("eq.internal") + " " + (parseInt(m[1], 10) + 1) + " · " + tr("eq." + m[2])
  return key
}

// The mic's noise reduction is two fields; the panel shows one 4-way choice.
function nrChoice(mic) {
  if (!mic || mic["mic.nr.on"] === null || mic["mic.nr.on"] === undefined) return null
  return mic["mic.nr.on"] ? "lvl:" + mic["mic.nr.level"] : "off"
}

function nrChanges(choice) {
  if (choice === "off") return { "mic.nr.on": false }
  return { "mic.nr.on": true, "mic.nr.level": parseInt(String(choice).slice(4), 10) }
}

function nrOptions(schema, tr) {
  return [{ value: "off", label: tr("nr.off") }].concat(enumOptions(schema, "mic.nr.level", tr).map(function (o) {
    return { value: "lvl:" + o.value, label: o.label }
  }))
}

function nextProfileId(profiles, active) {
  if (!profiles || profiles.length === 0) return null
  var i = -1
  for (var k = 0; k < profiles.length; k++) if (profiles[k].id === active) i = k
  return profiles[(i + 1) % profiles.length].id
}

function profileName(profiles, id) {
  for (var i = 0; i < (profiles || []).length; i++) if (profiles[i].id === id) return profiles[i].name
  return id
}

function langFor(setting, localeName, available) {
  if (setting && setting !== "auto" && available.indexOf(setting) >= 0) return setting
  var l = String(localeName || "").slice(0, 2)
  return available.indexOf(l) >= 0 ? l : "en"
}

function t(strings, lang, key, params) {
  var table = strings[lang] || {}
  var s = table[key] !== undefined ? table[key] : (strings.en && strings.en[key] !== undefined ? strings.en[key] : key)
  if (params) for (var p in params) s = s.split("{" + p + "}").join(String(params[p]))
  return s
}

// What to tell the user before anything else; "" when all is well.
function banner(hasService, status) {
  if (!hasService) return "banner.noService"
  if (!status.running) return "banner.noBackend"
  if (!status.ready || !status.st) return "banner.connecting"
  var d = status.st.device
  if (d === "disconnected") return "banner.disconnected"
  if (d === "permission") return "banner.permission"
  if (d === "unsupported") return "banner.unsupported"
  if (status.st.filterApplied === false) return "banner.filterDown"
  return ""
}

function liveRefusal(ack) {
  if (!ack || ack.ok) return ""
  return ack.error === "not-headphones" ? "force" : "error"
}

function ackText(ack) {
  if (!ack) return ""
  if (ack.message) return ack.message
  var s = String(ack.error || "")
  if (ack.details && ack.details.length) s += ": " + ack.details.join("; ")
  return s
}

function formatValue(v, unit) {
  if (v === null || v === undefined || isNaN(v)) return "—"
  if (unit === "Hz") return formatHz(v)
  var n = Math.abs(v - Math.round(v)) < 1e-9 ? String(Math.round(v)) : String(Math.round(v * 10) / 10)
  return unit ? n + " " + unit : n
}

function formatHz(f) {
  return f >= 1000 ? (Math.round(f / 100) / 10) + " kHz" : Math.round(f) + " Hz"
}

// Log mapping for frequency sliders: position 0..1000 <-> Hz.
function freqToPos(f, lo, hi) { return Math.round(1000 * Math.log(clamp(f, lo, hi) / lo) / Math.log(hi / lo)) }
function posToFreq(p, lo, hi) { return Math.round(lo * Math.pow(hi / lo, clamp(p, 0, 1000) / 1000)) }

function hsvToHex(hsv) {
  if (!hsv || hsv.length !== 3) return "#000000"
  var h = ((hsv[0] % 360) + 360) % 360, s = hsv[1], v = hsv[2]
  var c = v * s, x = c * (1 - Math.abs((h / 60) % 2 - 1)), m = v - c
  var rgb = h < 60 ? [c, x, 0] : h < 120 ? [x, c, 0] : h < 180 ? [0, c, x] : h < 240 ? [0, x, c] : h < 300 ? [x, 0, c] : [c, 0, x]
  return "#" + rgb.map(function (u) { var n = Math.round((u + m) * 255); return (n < 16 ? "0" : "") + n.toString(16) }).join("")
}

// Nerd Font glyphs by role (code points, so no tool ever strips the characters).
var GLYPHS = { mic: 0xF036C, micOff: 0xF036D, voice: 0xF036C, eq: 0xF0EA2, filter: 0xF0232, light: 0xF0335, profiles: 0xF0279, settings: 0xF0493, live: 0xF02CB, warn: 0xF0026, apply: 0xF040A, duplicate: 0xF018F, rename: 0xF03EB, remove: 0xF01B4, fold: 0xF0142, unfold: 0xF0140 }
function glyph(name) { return GLYPHS[name] ? String.fromCodePoint(GLYPHS[name]) : "" }

// UI preferences are declared in manifest.json (barWidget.schema); defaults and ranges come from there.
function prefSpec(manifest, key) {
  var list = manifest && manifest.barWidget && manifest.barWidget.schema ? manifest.barWidget.schema : []
  for (var i = 0; i < list.length; i++) if (list[i].key === key) return list[i]
  return null
}

function prefDefault(manifest, key, fallback) {
  var s = prefSpec(manifest, key)
  return s && s.defaultValue !== undefined ? s.defaultValue : fallback
}

function barTooltip(tr, st, profiles) {
  if (!st || st.device !== "connected") return tr("tooltip.offline")
  var m = st.mic || {}
  var parts = [tr(m["mic.mute"] === true ? "tooltip.muted" : "tooltip.live")]
  if (typeof m["info.battery"] === "number") parts.push(tr("hero.battery", { pct: m["info.battery"] }))
  if (typeof m["mic.gain"] === "number") parts.push(tr("voice.gain") + " " + m["mic.gain"])
  if (st.activeProfile) parts.push(profileName(profiles, st.activeProfile))
  return parts.join(" · ")
}
```

The glyph code points are Nerd Font Material Design icons. Task 15 confirms visually that each one renders; a wrong code point is a one-number fix in `GLYPHS`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `node --test shell/tests/`
Expected: `# pass 19`, `# fail 0`.

- [ ] **Step 5: Commit**

```bash
git add shell/Model.js shell/tests/model.test.js
git commit -m "feat(panel): Model.js protocol reducer, optimistic edits and schema helpers

Co-Authored-By: <your model's trailer>"
```

(Every commit in this plan ends with the Co-Authored-By trailer your own harness gives you.)

---

### Task 2: `Model.js` filter-chain frequency response (for the EQ curve)

**Files:**
- Modify: `shell/Model.js`
- Create: `shell/tests/eq.test.js`

**Interfaces:**
- Produces:
  - `biquadCoeffs(type, f0, q, gainDb, fs) -> [b0, b1, b2, a1, a2]`, normalized RBJ cookbook coefficients. `type` is one of `peak|lowshelf|highshelf|highpass|lowpass`.
  - `biquadDb(k, f, fs) -> dB`
  - `chainResponse(filterState, samples) -> [{x: 0..1 (log 20 Hz..20 kHz), db}]`. It mirrors the backend's bypass rules: the HPF counts when `enabled && hpf.on`; the bands and LPF count when `enabled && eq.on`, with the LPF also needing `lpf.on`. The HPF is 2 stages at Q 0.707.

- [ ] **Step 1: Write the failing test**

`shell/tests/eq.test.js`:

```js
const test = require("node:test")
const assert = require("node:assert/strict")
const fs = require("node:fs")
const path = require("node:path")
const vm = require("node:vm")

const M = {}
vm.createContext(M)
vm.runInContext(fs.readFileSync(path.join(__dirname, "..", "Model.js"), "utf8"), M, { filename: "Model.js" })

const flat = () => ({
  enabled: true,
  hpf: { on: false, freq: 90 },
  eq: { on: true, preset: null, bands: [125, 250, 500, 1000, 2000].map((f) => ({ type: "peak", freq: f, gain: 0, q: 1 })) },
  lpf: { on: false, freq: 20000 },
})
const at = (pts, hz) => pts.reduce((best, p) => {
  const f = 20 * Math.pow(1000, p.x)
  return Math.abs(Math.log(f / hz)) < Math.abs(Math.log(best.f / hz)) ? { f, db: p.db } : best
}, { f: 1e9, db: NaN }).db

test("flat chain is 0 dB everywhere", () => {
  for (const p of M.chainResponse(flat(), 64)) assert.ok(Math.abs(p.db) < 0.01, JSON.stringify(p))
})

test("a +6 dB peak shows up at its frequency", () => {
  const s = flat()
  s.eq.bands[3].gain = 6
  const pts = M.chainResponse(s, 200)
  assert.ok(Math.abs(at(pts, 1000) - 6) < 0.3, String(at(pts, 1000)))
  assert.ok(Math.abs(at(pts, 50)) < 0.5)
})

test("the 2-stage high-pass cuts lows and leaves mids", () => {
  const s = flat()
  s.hpf = { on: true, freq: 90 }
  const pts = M.chainResponse(s, 200)
  assert.ok(at(pts, 20) < -20, String(at(pts, 20)))
  assert.ok(Math.abs(at(pts, 1000)) < 0.5)
})

test("bypass rules mirror the backend", () => {
  const s = flat()
  s.eq.bands[3].gain = 6
  s.hpf = { on: true, freq: 90 }
  s.eq.on = false
  const noEq = M.chainResponse(s, 100)
  assert.ok(Math.abs(at(noEq, 1000)) < 0.5, "eq.on=false bypasses the bands")
  assert.ok(at(noEq, 20) < -20, "but the HPF stays")
  s.enabled = false
  for (const p of M.chainResponse(s, 50)) assert.ok(Math.abs(p.db) < 0.01)
})

test("shelves and low-pass move the right end", () => {
  const s = flat()
  s.eq.bands[4] = { type: "highshelf", freq: 8000, gain: -10, q: 0.7 }
  s.lpf = { on: true, freq: 10000 }
  const pts = M.chainResponse(s, 200)
  assert.ok(at(pts, 18000) < -10)
  assert.ok(Math.abs(at(pts, 200)) < 0.5)
})
```

- [ ] **Step 2: Run to verify it fails**

Run: `node --test shell/tests/eq.test.js`
Expected: FAIL with `M.chainResponse is not a function`.

- [ ] **Step 3: Implement**

Append to `shell/Model.js`:

```js
// RBJ cookbook biquad, normalized: [b0, b1, b2, a1, a2] (a0 = 1).
function biquadCoeffs(type, f0, q, gainDb, fs) {
  var A = Math.pow(10, gainDb / 40), w = 2 * Math.PI * f0 / fs, c = Math.cos(w), sn = Math.sin(w), al = sn / (2 * q)
  var b0, b1, b2, a0, a1, a2, sq
  if (type === "peak") {
    b0 = 1 + al * A; b1 = -2 * c; b2 = 1 - al * A; a0 = 1 + al / A; a1 = -2 * c; a2 = 1 - al / A
  } else if (type === "lowshelf") {
    sq = 2 * Math.sqrt(A) * al
    b0 = A * ((A + 1) - (A - 1) * c + sq); b1 = 2 * A * ((A - 1) - (A + 1) * c); b2 = A * ((A + 1) - (A - 1) * c - sq)
    a0 = (A + 1) + (A - 1) * c + sq; a1 = -2 * ((A - 1) + (A + 1) * c); a2 = (A + 1) + (A - 1) * c - sq
  } else if (type === "highshelf") {
    sq = 2 * Math.sqrt(A) * al
    b0 = A * ((A + 1) + (A - 1) * c + sq); b1 = -2 * A * ((A - 1) + (A + 1) * c); b2 = A * ((A + 1) + (A - 1) * c - sq)
    a0 = (A + 1) - (A - 1) * c + sq; a1 = 2 * ((A - 1) - (A + 1) * c); a2 = (A + 1) - (A - 1) * c - sq
  } else if (type === "highpass") {
    b0 = (1 + c) / 2; b1 = -(1 + c); b2 = (1 + c) / 2; a0 = 1 + al; a1 = -2 * c; a2 = 1 - al
  } else {
    b0 = (1 - c) / 2; b1 = 1 - c; b2 = (1 - c) / 2; a0 = 1 + al; a1 = -2 * c; a2 = 1 - al
  }
  return [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

function biquadDb(k, f, fs) {
  var w = 2 * Math.PI * f / fs
  var c1 = Math.cos(w), s1 = -Math.sin(w), c2 = Math.cos(2 * w), s2 = -Math.sin(2 * w)
  var nr = k[0] + k[1] * c1 + k[2] * c2, ni = k[1] * s1 + k[2] * s2
  var dr = 1 + k[3] * c1 + k[4] * c2, di = k[3] * s1 + k[4] * s2
  return 10 * Math.log10((nr * nr + ni * ni) / (dr * dr + di * di))
}

// Magnitude of the cleanup chain on a log axis 20 Hz..20 kHz, with the backend's bypass rules.
function chainResponse(s, samples) {
  var fs = 48000, stages = [], out = []
  if (s && s.enabled) {
    if (s.hpf && s.hpf.on) {
      var hp = biquadCoeffs("highpass", s.hpf.freq, 0.707, 0, fs)
      stages.push(hp, hp)
    }
    if (s.eq && s.eq.on) {
      for (var b = 0; b < (s.eq.bands || []).length; b++) {
        var band = s.eq.bands[b]
        stages.push(biquadCoeffs(band.type, band.freq, band.q, band.gain, fs))
      }
      if (s.lpf && s.lpf.on) stages.push(biquadCoeffs("lowpass", s.lpf.freq, 0.707, 0, fs))
    }
  }
  for (var i = 0; i <= samples; i++) {
    var x = i / samples, f = 20 * Math.pow(1000, x), db = 0
    for (var j = 0; j < stages.length; j++) db += biquadDb(stages[j], f, fs)
    out.push({ x: x, db: db })
  }
  return out
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `node --test shell/tests/`
Expected: all pass (19 + 5).

- [ ] **Step 5: Commit**

```bash
git add shell/Model.js shell/tests/eq.test.js
git commit -m "feat(panel): exact biquad response of the filter chain for the EQ curve"
```

---

### Task 3: `Strings.js` (Spanish + English UI text)

**Files:**
- Create: `shell/Strings.js`
- Create: `shell/tests/strings.test.js`

**Interfaces:**
- Produces: `STRINGS = { es: {key: text}, en: {key: text} }`. Placeholders are written `{name}`.
- **Key families:**
  - `hero.*`, `banner.*`, `notice.*`, `error.*`, `live.*`, `tab.*`, `voice.*`, `nr.*`, `monitor.*`, `effect.*`, `badge.*`
  - `field.<descriptor key>`, `eq.*`, `filter.*`, `comp.*`, `light.*`, `color.<preset name>`
  - `profiles.*`, `group.*`, `settings.*`, `action.*`, `lang.*`, `info.*`, `footer.keys`, `tooltip.*`
- `strings.test.js` fails when a key used by any QML/JS file, the descriptor, the filter schema or the manifest is missing in either language, or when the two tables differ.

- [ ] **Step 1: Write the failing test**

`shell/tests/strings.test.js`:

```js
const test = require("node:test")
const assert = require("node:assert/strict")
const fs = require("node:fs")
const path = require("node:path")
const vm = require("node:vm")

const shellDir = path.join(__dirname, "..")
const S = {}
vm.createContext(S)
vm.runInContext(fs.readFileSync(path.join(shellDir, "Strings.js"), "utf8"), S, { filename: "Strings.js" })
const repo = path.join(shellDir, "..")
const json = (p) => JSON.parse(fs.readFileSync(path.join(repo, p), "utf8"))

test("es_and_en_have_the_same_keys", () => {
  const es = Object.keys(S.STRINGS.es).sort()
  const en = Object.keys(S.STRINGS.en).sort()
  assert.deepEqual(es.filter((k) => !en.includes(k)), [], "only in es")
  assert.deepEqual(en.filter((k) => !es.includes(k)), [], "only in en")
  for (const k of es) assert.ok(S.STRINGS.es[k] && S.STRINGS.en[k], "empty text for " + k)
})

test("placeholders match between languages", () => {
  const ph = (t) => (t.match(/\{\w+\}/g) || []).sort().join(",")
  for (const k of Object.keys(S.STRINGS.es)) assert.equal(ph(S.STRINGS.es[k]), ph(S.STRINGS.en[k]), k)
})

test("every_key_used_in_qml_exists", () => {
  const used = new Set()
  for (const f of fs.readdirSync(shellDir).filter((f) => f.endsWith(".qml") || f === "Model.js")) {
    const src = fs.readFileSync(path.join(shellDir, f), "utf8")
    for (const m of src.matchAll(/tr\("([\w.]*\w)"/g)) used.add(m[1])
    for (const m of src.matchAll(/"((?:banner|notice|error|info|tooltip|hero|voice)\.\w+)"/g)) used.add(m[1])
  }
  const device = json("devices/pd100w.json")
  const filter = json("devices/filter.json")
  const manifest = JSON.parse(fs.readFileSync(path.join(shellDir, "manifest.json"), "utf8"))
  for (const t of ["voice", "eq", "filter", "light", "profiles", "settings"]) used.add("tab." + t)
  for (const p of device.light.presets) used.add("color." + p.name)
  for (const f of device.fields) {
    if (f.key !== "mic.mute" && !f.key.startsWith("mic.nr.")) used.add("field." + f.key)
    for (const o of f.options || []) used.add(o.label)
  }
  for (const sf of device.eqSlots.fields) { used.add("eq." + sf.name); for (const o of sf.options || []) used.add(o.label) }
  for (const f of filter.fields) for (const o of f.options || []) used.add("eq." + o)
  for (const p of manifest.barWidget.schema) {
    if (p.key === "middleClick" || p.key === "rightClick") for (const o of p.options) used.add("action." + o)
    if (p.key === "language") for (const o of p.options) used.add("lang." + o)
  }
  for (const code of ["device", "identity", "filter", "monitor"]) used.add("error." + code)
  const missing = [...used].filter((k) => !(k in S.STRINGS.es) || !(k in S.STRINGS.en)).sort()
  assert.deepEqual(missing, [])
})
```

- [ ] **Step 2: Run to verify it fails**

Run: `node --test shell/tests/strings.test.js`
Expected: FAIL with `ENOENT ... Strings.js`.

- [ ] **Step 3: Write `shell/Strings.js`**

```js
// UI text, Spanish and English, same keys. Placeholders: {name}.
// Only a top-level var (Node vm harness; see shell/tests/strings.test.js).

var STRINGS = {
  es: {
    "hero.title": "Maono PD100W",
    "hero.battery": "batería {pct}%",
    "hero.charging": "cargando",
    "hero.firmware": "firmware {v}",
    "hero.live": "Live",
    "hero.liveClean": "Limpio",
    "hero.liveRaw": "Crudo",
    "hero.meter": "Nivel · {source}",
    "hero.clip": "satura",
    "hero.profile": "Perfil",
    "hero.dirty": "cambios sin guardar",
    "hero.partial": "perfil aplicado a medias, falló: {groups}",
    "hero.reapply": "Reaplicar",
    "hero.saveChanges": "Guardar cambios",
    "banner.noService": "El servicio del plugin no está cargado",
    "banner.noBackend": "No corre maono serve (¿está instalado en ~/.local/bin?)",
    "banner.connecting": "Conectando con maono serve…",
    "banner.disconnected": "El mic está desconectado",
    "banner.permission": "Sin permiso para el mic: falta la regla udev 70-maono.rules",
    "banner.unsupported": "Hay un dispositivo Maono que no es un PD100W",
    "banner.filterDown": "El filtro de PipeWire no está activo",
    "notice.protocol": "Versión de maono incompatible (protocolo {got})",
    "notice.reapplied": "Se reaplicó el perfil {profile} al reconectar",
    "notice.reappliedRecover": "Se reaplicó {profile} al reconectar; tenías cambios sin guardar",
    "notice.recover": "Recuperar cambios",
    "notice.dismiss": "Cerrar",
    "error.ack": "No se pudo",
    "error.device": "Error del mic",
    "error.identity": "Dispositivo no reconocido",
    "error.filter": "El filtro no arrancó",
    "error.monitor": "Live se apagó: la salida ya no son auriculares",
    "error.otherServe": "Ya hay otro maono serve corriendo",
    "error.serveExited": "maono serve terminó (código {code}); reintentando",
    "live.notHeadphones": "La salida no parece auriculares: por parlantes se acopla. ¿Forzar igual?",
    "live.force": "Forzar",
    "tab.voice": "Voz",
    "tab.eq": "EQ",
    "tab.filter": "Filtro",
    "tab.light": "Luz",
    "tab.profiles": "Perfiles",
    "tab.settings": "Ajustes",
    "voice.title": "Voz",
    "voice.gain": "Ganancia",
    "voice.nr": "Supresión del mic",
    "voice.headphones": "Auriculares del mic",
    "voice.dsp": "DSP del mic",
    "voice.dspEq": "EQ interno del mic",
    "nr.off": "off",
    "nr.low": "baja",
    "nr.mid": "media",
    "nr.high": "alta",
    "monitor.none": "nada",
    "monitor.voice": "voz",
    "monitor.pc": "PC",
    "monitor.both": "voz + PC",
    "monitor.all": "todo",
    "effect.fixed": "fija",
    "effect.breathe": "respira",
    "effect.cycle": "ciclo",
    "badge.experimental": "experimental",
    "badge.unverified": "no verificado",
    "field.mic.gain": "Ganancia",
    "field.headphones.volume": "Volumen de auriculares",
    "field.headphones.monitor": "Monitoreo",
    "field.light.on": "Luz",
    "field.light.brightness": "Brillo",
    "field.light.effect": "Efecto",
    "field.dsp.comp.on": "Compresor del mic",
    "field.dsp.comp.threshold": "Umbral del compresor",
    "field.dsp.comp.attack": "Ataque del compresor",
    "field.dsp.comp.release": "Liberación del compresor",
    "field.dsp.comp.ratio": "Ratio del compresor",
    "field.dsp.limiter.on": "Limitador del mic",
    "field.dsp.limiter.threshold": "Umbral del limitador",
    "field.dsp.limiter.attack": "Ataque del limitador",
    "field.dsp.limiter.release": "Liberación del limitador",
    "field.dsp.reverb.on": "Reverb",
    "field.dsp.reverb.level": "Nivel de reverb",
    "eq.title": "Ecualizador",
    "eq.presets": "Presets",
    "eq.custom": "personalizado",
    "eq.hpf": "Pasa-altos (corta golpes)",
    "eq.hpfFreq": "Corte del pasa-altos",
    "eq.lpf": "Pasa-bajos",
    "eq.lpfFreq": "Corte del pasa-bajos",
    "eq.bands": "Bandas",
    "eq.band": "Banda {n}",
    "eq.type": "Tipo",
    "eq.peak": "campana",
    "eq.lowshelf": "graves",
    "eq.highshelf": "agudos",
    "eq.lowpass": "pasa-bajos",
    "eq.highpass": "pasa-altos",
    "eq.enable": "activa",
    "eq.freq": "Frecuencia",
    "eq.gain": "Ganancia",
    "eq.q": "Q",
    "eq.internal": "Banda interna",
    "eq.savePreset": "Guardar preset",
    "eq.presetName": "Nombre del preset",
    "eq.deletePreset": "Borrar este preset",
    "filter.title": "Filtro de PipeWire",
    "filter.enabled": "Filtro activo",
    "filter.source": "Entrada predeterminada",
    "filter.sourceClean": "limpia",
    "filter.sourceRaw": "cruda",
    "filter.sourceNote": "Las apps que eligieron su propia entrada (Discord, Meet) no cambian con esto.",
    "filter.rnnoise": "Supresión de ruido (RNNoise)",
    "filter.vad": "Umbral de voz",
    "filter.grace": "Gracia",
    "filter.retro": "Gracia retroactiva",
    "filter.retroNote": "agrega latencia",
    "filter.comp": "Compresor",
    "filter.compOn": "Compresor activo",
    "filter.missing": "Falta {pkg}: sudo pacman -S {pkg}",
    "comp.threshold": "Umbral",
    "comp.ratio": "Ratio",
    "comp.attack": "Ataque",
    "comp.release": "Liberación",
    "comp.makeup": "Compensación",
    "light.title": "Luz",
    "light.colors": "Colores",
    "light.custom": "Colores propios ({n}/{max})",
    "light.cycleNote": "En el efecto ciclo los colores rotan solos.",
    "light.editor": "Editor de color",
    "light.hue": "Tono",
    "light.sat": "Saturación",
    "light.val": "Intensidad",
    "light.add": "Agregar color",
    "light.update": "Guardar en #{n}",
    "light.delete": "Borrar #{n}",
    "color.white": "blanco",
    "color.red": "rojo",
    "color.orange": "naranja",
    "color.yellow": "amarillo",
    "color.green": "verde",
    "color.cyan": "cian",
    "color.blue": "azul",
    "color.magenta": "magenta",
    "profiles.title": "Perfiles",
    "profiles.active": "activo",
    "profiles.apply": "Aplicar",
    "profiles.duplicate": "Duplicar",
    "profiles.rename": "Renombrar",
    "profiles.delete": "Borrar",
    "profiles.confirmDelete": "¿Borrar? Tocá otra vez",
    "profiles.empty": "No hay perfiles todavía.",
    "profiles.save": "Guardar estado actual como…",
    "profiles.name": "Nombre del perfil",
    "profiles.saveButton": "Guardar",
    "profiles.groups": "Incluir",
    "profiles.reapplyOnReconnect": "Reaplicar al reconectar",
    "profiles.reapplyOnReconnectHint": "Al volver a enchufar el mic se reaplica el perfil activo (nunca toca el mute).",
    "profiles.copy": "Copiar atajo: {cmd}",
    "group.mic": "mic",
    "group.filter": "filtro",
    "group.light": "luz",
    "settings.title": "Ajustes",
    "settings.middle": "Clic medio en la barra",
    "settings.right": "Clic derecho en la barra",
    "settings.wheelStep": "Paso de la rueda (ganancia)",
    "settings.hide": "Ocultar si el mic está desconectado",
    "settings.battery": "Aviso de batería baja en el ícono",
    "settings.language": "Idioma",
    "settings.experimental": "Mostrar DSP experimental del mic",
    "settings.experimentalHint": "Controles internos del mic que no cambiaron el audio en las pruebas.",
    "settings.device": "Equipo",
    "settings.diagnostics": "Diagnóstico",
    "settings.noWarnings": "Sin avisos.",
    "settings.restart": "Reiniciar maono serve",
    "action.mute": "mute",
    "action.nextProfile": "perfil siguiente",
    "action.live": "Live",
    "action.none": "nada",
    "lang.auto": "auto",
    "lang.es": "español",
    "lang.en": "English",
    "info.battery": "Batería",
    "info.charging": "Cargando",
    "info.model": "Modelo",
    "info.firmware": "Firmware",
    "info.serial": "Serie",
    "info.source": "Fuente limpia",
    "info.defaultSource": "Entrada predeterminada",
    "info.filter": "Filtro",
    "info.filterOk": "activo",
    "info.filterDown": "caído",
    "info.binary": "Binario",
    "footer.keys": "j/k mover · h/l ajustar · Enter activar · 1-6 pestañas · m mute · v live · r reaplicar",
    "tooltip.live": "Mic en vivo",
    "tooltip.muted": "Mic en mute",
    "tooltip.offline": "Mic desconectado"
  },
  en: {
    "hero.title": "Maono PD100W",
    "hero.battery": "battery {pct}%",
    "hero.charging": "charging",
    "hero.firmware": "firmware {v}",
    "hero.live": "Live",
    "hero.liveClean": "Clean",
    "hero.liveRaw": "Raw",
    "hero.meter": "Level · {source}",
    "hero.clip": "clipping",
    "hero.profile": "Profile",
    "hero.dirty": "unsaved changes",
    "hero.partial": "profile partly applied, failed: {groups}",
    "hero.reapply": "Reapply",
    "hero.saveChanges": "Save changes",
    "banner.noService": "The plugin service is not loaded",
    "banner.noBackend": "maono serve is not running (is it installed in ~/.local/bin?)",
    "banner.connecting": "Connecting to maono serve…",
    "banner.disconnected": "The mic is disconnected",
    "banner.permission": "No permission for the mic: the 70-maono.rules udev rule is missing",
    "banner.unsupported": "A Maono device that is not a PD100W is plugged in",
    "banner.filterDown": "The PipeWire filter is not running",
    "notice.protocol": "Incompatible maono version (protocol {got})",
    "notice.reapplied": "Profile {profile} reapplied after reconnecting",
    "notice.reappliedRecover": "{profile} reapplied after reconnecting; you had unsaved changes",
    "notice.recover": "Recover changes",
    "notice.dismiss": "Dismiss",
    "error.ack": "Could not",
    "error.device": "Mic error",
    "error.identity": "Device not recognised",
    "error.filter": "The filter did not start",
    "error.monitor": "Live stopped: the output is no longer headphones",
    "error.otherServe": "Another maono serve is already running",
    "error.serveExited": "maono serve exited (code {code}); retrying",
    "live.notHeadphones": "The output does not look like headphones: speakers would feed back. Force it anyway?",
    "live.force": "Force",
    "tab.voice": "Voice",
    "tab.eq": "EQ",
    "tab.filter": "Filter",
    "tab.light": "Light",
    "tab.profiles": "Profiles",
    "tab.settings": "Settings",
    "voice.title": "Voice",
    "voice.gain": "Gain",
    "voice.nr": "Mic noise reduction",
    "voice.headphones": "Mic headphones",
    "voice.dsp": "Mic DSP",
    "voice.dspEq": "Mic internal EQ",
    "nr.off": "off",
    "nr.low": "low",
    "nr.mid": "mid",
    "nr.high": "high",
    "monitor.none": "none",
    "monitor.voice": "voice",
    "monitor.pc": "PC",
    "monitor.both": "voice + PC",
    "monitor.all": "all",
    "effect.fixed": "fixed",
    "effect.breathe": "breathe",
    "effect.cycle": "cycle",
    "badge.experimental": "experimental",
    "badge.unverified": "unverified",
    "field.mic.gain": "Gain",
    "field.headphones.volume": "Headphone volume",
    "field.headphones.monitor": "Monitoring",
    "field.light.on": "Light",
    "field.light.brightness": "Brightness",
    "field.light.effect": "Effect",
    "field.dsp.comp.on": "Mic compressor",
    "field.dsp.comp.threshold": "Compressor threshold",
    "field.dsp.comp.attack": "Compressor attack",
    "field.dsp.comp.release": "Compressor release",
    "field.dsp.comp.ratio": "Compressor ratio",
    "field.dsp.limiter.on": "Mic limiter",
    "field.dsp.limiter.threshold": "Limiter threshold",
    "field.dsp.limiter.attack": "Limiter attack",
    "field.dsp.limiter.release": "Limiter release",
    "field.dsp.reverb.on": "Reverb",
    "field.dsp.reverb.level": "Reverb level",
    "eq.title": "Equalizer",
    "eq.presets": "Presets",
    "eq.custom": "custom",
    "eq.hpf": "High-pass (cuts thumps)",
    "eq.hpfFreq": "High-pass cutoff",
    "eq.lpf": "Low-pass",
    "eq.lpfFreq": "Low-pass cutoff",
    "eq.bands": "Bands",
    "eq.band": "Band {n}",
    "eq.type": "Type",
    "eq.peak": "peak",
    "eq.lowshelf": "low shelf",
    "eq.highshelf": "high shelf",
    "eq.lowpass": "low-pass",
    "eq.highpass": "high-pass",
    "eq.enable": "enabled",
    "eq.freq": "Frequency",
    "eq.gain": "Gain",
    "eq.q": "Q",
    "eq.internal": "Internal band",
    "eq.savePreset": "Save preset",
    "eq.presetName": "Preset name",
    "eq.deletePreset": "Delete this preset",
    "filter.title": "PipeWire filter",
    "filter.enabled": "Filter on",
    "filter.source": "Default input",
    "filter.sourceClean": "clean",
    "filter.sourceRaw": "raw",
    "filter.sourceNote": "Apps that picked their own input (Discord, Meet) are not changed by this.",
    "filter.rnnoise": "Noise suppression (RNNoise)",
    "filter.vad": "Voice threshold",
    "filter.grace": "Grace",
    "filter.retro": "Retroactive grace",
    "filter.retroNote": "adds latency",
    "filter.comp": "Compressor",
    "filter.compOn": "Compressor on",
    "filter.missing": "{pkg} is missing: sudo pacman -S {pkg}",
    "comp.threshold": "Threshold",
    "comp.ratio": "Ratio",
    "comp.attack": "Attack",
    "comp.release": "Release",
    "comp.makeup": "Makeup",
    "light.title": "Light",
    "light.colors": "Colours",
    "light.custom": "Custom colours ({n}/{max})",
    "light.cycleNote": "In the cycle effect the colours rotate on their own.",
    "light.editor": "Colour editor",
    "light.hue": "Hue",
    "light.sat": "Saturation",
    "light.val": "Value",
    "light.add": "Add colour",
    "light.update": "Save to #{n}",
    "light.delete": "Delete #{n}",
    "color.white": "white",
    "color.red": "red",
    "color.orange": "orange",
    "color.yellow": "yellow",
    "color.green": "green",
    "color.cyan": "cyan",
    "color.blue": "blue",
    "color.magenta": "magenta",
    "profiles.title": "Profiles",
    "profiles.active": "active",
    "profiles.apply": "Apply",
    "profiles.duplicate": "Duplicate",
    "profiles.rename": "Rename",
    "profiles.delete": "Delete",
    "profiles.confirmDelete": "Delete? Press again",
    "profiles.empty": "No profiles yet.",
    "profiles.save": "Save current state as…",
    "profiles.name": "Profile name",
    "profiles.saveButton": "Save",
    "profiles.groups": "Include",
    "profiles.reapplyOnReconnect": "Reapply on reconnect",
    "profiles.reapplyOnReconnectHint": "When the mic is plugged back in, the active profile is reapplied (mute is never touched).",
    "profiles.copy": "Copy shortcut: {cmd}",
    "group.mic": "mic",
    "group.filter": "filter",
    "group.light": "light",
    "settings.title": "Settings",
    "settings.middle": "Middle click on the bar",
    "settings.right": "Right click on the bar",
    "settings.wheelStep": "Wheel step (gain)",
    "settings.hide": "Hide when the mic is disconnected",
    "settings.battery": "Low-battery dot on the icon",
    "settings.language": "Language",
    "settings.experimental": "Show experimental mic DSP",
    "settings.experimentalHint": "Internal mic controls that did not change the audio in testing.",
    "settings.device": "Device",
    "settings.diagnostics": "Diagnostics",
    "settings.noWarnings": "No warnings.",
    "settings.restart": "Restart maono serve",
    "action.mute": "mute",
    "action.nextProfile": "next profile",
    "action.live": "Live",
    "action.none": "nothing",
    "lang.auto": "auto",
    "lang.es": "español",
    "lang.en": "English",
    "info.battery": "Battery",
    "info.charging": "Charging",
    "info.model": "Model",
    "info.firmware": "Firmware",
    "info.serial": "Serial",
    "info.source": "Clean source",
    "info.defaultSource": "Default input",
    "info.filter": "Filter",
    "info.filterOk": "running",
    "info.filterDown": "down",
    "info.binary": "Binary",
    "footer.keys": "j/k move · h/l adjust · Enter activate · 1-6 tabs · m mute · v live · r reapply",
    "tooltip.live": "Mic live",
    "tooltip.muted": "Mic muted",
    "tooltip.offline": "Mic disconnected"
  }
}
```

- [ ] **Step 4: Run to verify**

Run: `node --test shell/tests/`
Expected: all pass. `every_key_used_in_qml_exists` passes now (QML files come later); rerun it after every QML task. Tasks 5–13 must keep it green.

- [ ] **Step 5: Commit**

```bash
git add shell/Strings.js shell/tests/strings.test.js
git commit -m "feat(panel): Spanish and English strings with parity and coverage tests"
```

---

### Task 4: `Service.qml` (owns `maono serve`, protocol, optimistic edits, IPC)

**Files:**
- Create: `shell/Service.qml`

**Interfaces:**
- Consumes: Model.js (Task 1), Strings.js (Task 3: `STRINGS`)
- Produces (properties on the shared service object that BarWidget/Panel/tabs use):
  - State: `pluginId`, `shell`, `manifest`, `settings` (pushed by BarWidget), `status`, `pending`, `pendingFilter`, `ready`, `st`, `mic`, `connected`, `lang`, `binary`, `requestedTab`, `tabRequestSerial`
  - Functions:
    - `tr(key, params)`, `send(obj)`, `request(cmd, cb) -> id`, `report(ack)`
    - `value(key)`, `filterValue(path)`, `effectiveFilter()`
    - `setMic(changes)`, `setFilter(changes)`, `setConfig(changes)`
    - `toggleMute()`, `applyProfile(id)`, `nextProfile()`, `setMonitor(on, source, force, cb)`, `toggleLive()`
    - `dismissNotice()`, `restart()`
  - IPC target `maono`: `toggle()`, `toggleMute()`, `nextProfile()`, `applyProfile(id)`, `live()`, `tab(index)`

There is no QML unit test. Correctness rests on Model.js, which Task 1 covers, plus the qmllint syntax check and Task 15's live run.

- [ ] **Step 1: Write `shell/Service.qml`**

```qml
import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model
import "Strings.js" as Strings

// One per shell (manifest kind "service"). Owns `maono serve` and turns its
// JSON lines into bindable state; widgets and panels only read it and call it.
Item {
  id: root

  readonly property string pluginId: "io.github.agusmoura.maono"

  // injected by the host after creation (null in onCompleted)
  property var shell: null
  property var manifest: null
  // pushed by BarWidget instances; the host never injects settings into services
  property var settings: ({})

  property var status: Model.emptyStatus()
  property var pending: ({})
  property var pendingFilter: ({})
  property int requestedTab: -1
  property int tabRequestSerial: 0
  property int _restarts: 0
  property int _nextId: 1
  property var _callbacks: ({})

  readonly property bool ready: status.ready
  readonly property var st: status.st
  readonly property var mic: st && st.mic ? st.mic : ({})
  readonly property bool connected: !!st && st.device === "connected"
  readonly property string lang: Model.langFor(settings ? settings.language : "auto", Qt.locale().name, Object.keys(Strings.STRINGS))
  readonly property string binary: settings && settings.binary ? String(settings.binary) : Model.prefDefault(manifest, "binary", "maono")

  function tr(key, params) { return Model.t(Strings.STRINGS, root.lang, key, params) }

  function send(obj) {
    if (!daemon.running) return false
    daemon.write(JSON.stringify(obj) + "\n")
    return true
  }

  // Send a command with a fresh id; cb(ack) runs when its ack arrives (or at once if serve is down).
  function request(cmd, cb) {
    var id = root._nextId++
    var o = { id: id }
    for (var k in cmd) o[k] = cmd[k]
    if (cb) root._callbacks[id] = cb
    if (!root.send(o)) {
      delete root._callbacks[id]
      if (cb) cb({ ev: "ack", id: id, ok: false, error: "not-running" })
      return -1
    }
    return id
  }

  function report(ack) {
    if (ack && !ack.ok) root.status = Model.withNotice(root.status, { kind: "error", key: "error.ack", text: Model.ackText(ack) })
  }

  function value(key) { return Model.effective(root.pending, root.mic, key) }
  function filterValue(path) {
    if (Object.prototype.hasOwnProperty.call(root.pendingFilter, path)) return root.pendingFilter[path].value
    return Model.getPath(root.st ? root.st.filter : null, path)
  }
  function effectiveFilter() { return Model.overlayFilter(root.st ? root.st.filter : null, root.pendingFilter) }

  function setMic(changes) {
    var id = root.request({ cmd: "set", changes: changes }, root.report)
    if (id > 0) root.pending = Model.withPending(root.pending, changes, id)
  }
  function setFilter(changes) {
    var id = root.request({ cmd: "filter.set", changes: changes }, root.report)
    if (id > 0) root.pendingFilter = Model.withPending(root.pendingFilter, changes, id)
  }
  function setConfig(changes) {
    root.request({ cmd: "config.set", changes: changes }, function (ack) {
      if (ack.ok) root.status = Model.withConfig(root.status, changes)
      else root.report(ack)
    })
  }

  function toggleMute() {
    var m = root.value("mic.mute")
    if (m === true || m === false) root.setMic({ "mic.mute": !m })
  }
  function applyProfile(id) { root.request({ cmd: "profile.apply", profile: id }, root.report) }
  function nextProfile() {
    var id = Model.nextProfileId(root.status.profiles, root.st ? root.st.activeProfile : null)
    if (id) root.applyProfile(id)
  }
  function setMonitor(on, source, force, cb) {
    var c = { cmd: "monitor.set", on: on, force: !!force }
    if (source) c.source = source
    root.request(c, cb || root.report)
  }
  function toggleLive() {
    var on = !!root.st && !!root.st.monitor && root.st.monitor.on === true
    root.setMonitor(!on, null, false)
  }
  function dismissNotice() { root.status = Model.withNotice(root.status, null) }

  function handleLine(line) {
    var ev
    try { ev = JSON.parse(line) } catch (e) { console.warn(root.pluginId + ": bad line from serve:", line); return }
    if (ev.ev === "ack") {
      var cb = root._callbacks[ev.id]
      delete root._callbacks[ev.id]
      root.status = Model.applyAck(root.status, ev)
      root.pending = Model.settlePending(root.pending, ev.id)
      root.pendingFilter = Model.settlePending(root.pendingFilter, ev.id)
      if (cb) cb(ev)
      return
    }
    if (ev.ev === "hello") root._restarts = 0
    root.status = Model.applyEvent(root.status, ev)
  }

  function restart() {
    root._restarts = 0
    if (daemon.running) daemon.running = false
    restartTimer.interval = 500
    restartTimer.restart()
  }

  property string _startedWith: ""
  // A changed `binary` preference restarts serve; the first settings push (same value) does not.
  onBinaryChanged: if (daemon.running && root._startedWith !== "" && root._startedWith !== root.binary) root.restart()

  Process {
    id: daemon
    command: [root.binary, "serve"]
    stdinEnabled: true
    running: true
    onStarted: root._startedWith = root.binary
    stdout: SplitParser { onRead: function (line) { root.handleLine(line) } }
    stderr: SplitParser { onRead: function (line) { console.warn(root.pluginId + " serve:", line) } }
    onExited: function (exitCode, exitStatus) {
      root.status = Model.backendExited(root.status, exitCode)
      root.pending = ({})
      root.pendingFilter = ({})
      var cbs = root._callbacks
      root._callbacks = ({})
      for (var k in cbs) cbs[k]({ ev: "ack", id: Number(k), ok: false, error: "serve exited" })
      root._restarts += 1
      restartTimer.interval = Math.min(60000, 2000 * Math.pow(2, Math.min(root._restarts, 5)))
      restartTimer.restart()
    }
  }

  Timer {
    id: restartTimer
    repeat: false
    onTriggered: if (!daemon.running) daemon.running = true
  }

  // Keybinds: omarchy-shell maono toggleMute | nextProfile | applyProfile <id> | live | tab <n> | toggle
  IpcHandler {
    target: "maono"
    function toggle(): void { if (root.shell) root.shell.toggle(root.pluginId, "{}") }
    function toggleMute(): void { root.toggleMute() }
    function nextProfile(): void { root.nextProfile() }
    function applyProfile(id: string): void { root.applyProfile(id) }
    function live(): void { root.toggleLive() }
    function tab(index: int): void {
      root.requestedTab = index
      root.tabRequestSerial++
      if (root.shell) root.shell.summon(root.pluginId, "{}")
    }
  }
}
```

`_startedWith` records the binary each run used, so the first settings push (same value) never restarts serve.

- [ ] **Step 2: Syntax check**

Run: `/usr/lib/qt6/bin/qmllint shell/Service.qml 2>&1 | grep '\[syntax\]'`
Expected: no output.

- [ ] **Step 3: Commit**

```bash
git add shell/Service.qml
git commit -m "feat(panel): shared Service owning maono serve with optimistic edits and IPC"
```

---

### Task 5: Building-block controls

**Files (all new, in `shell/`):**
- `QuietSlider.qml`
- `SliderRow.qml`
- `ToggleRow.qml`
- `ChipRow.qml`
- `Fold.qml`
- `TabStrip.qml`
- `NameRow.qml`
- `ActionRow.qml`
- `HintText.qml`
- `EqCurve.qml`
- `MicSliderRow.qml`
- `MicToggleRow.qml`
- `MicChipRow.qml`
- `FilterSliderRow.qml`
- `FilterToggleRow.qml`
- `FieldRows.qml`

**Interfaces.**

Every control has `property var panel`. It reads `panel.foreground`, `dim`, `accent`, `urgent`, `fontFamily`, `bar`, `tr()`, `schema`, `service`, `status`, `connected`, `hoverRow(item)`, `refocus()` and `editing`.

Every keyboard row exposes:
- `hasCursor` (plain bool; the panel sets it imperatively)
- `adjust(dir)`
- `activate()`
- optionally `remove()`

Per control:
- `SliderRow`
  - properties: `label`, `badge`, `value`, `known`, `minimum`, `maximum`, `step`, `unit`, `logScale`, `interactive`, `format`
  - signal: `committed(real value)`, debounced to 120 ms while dragging, immediate on release or keyboard
- `ToggleRow`
  - properties: `label`, `description`, `badge`, `checked`, `interactive`
  - signal: `toggledTo(bool value)`
- `ChipRow`
  - properties: `label`, `options: [{value, label, color?}]`, `value`, `multi`, `swatches`, `interactive`
  - signal: `picked(var value)`
- `Fold`: properties `title`, `badge`, `open`; the default property holds the content.
- `TabStrip`: properties `tabs: [{key, glyph, label}]`, `current`; signal `picked(string key)`.
- `NameRow`: properties `placeholder`, `buttonText`, `text`; signal `submitted(string text)`.
- `ActionRow`: properties `text`, `interactive`, `destructive`; signal `triggered()`.
- `HintText`: a `Text` in caption size, dim, wrapped.
- `EqCurve`: property `filter` (FilterState); draws `Model.chainResponse`.
- `MicSliderRow`, `MicToggleRow`, `MicChipRow`: property `key` (descriptor key). They read `service.value(key)` and write `service.setMic`.
- `FilterSliderRow`, `FilterToggleRow`: property `path` (filter path). They read `service.filterValue(path)` and write `service.setFilter`.
- `FieldRows`
  - property `keys` (descriptor keys)
  - renders a Mic* row per key according to its type
  - function `rowItems()` returns the created rows in order

- [ ] **Step 1: `QuietSlider.qml`** (adapted from the Sony plugin; real-valued, no wheel)

```qml
import QtQuick
import qs.Commons

// A slider that ignores the mouse wheel, so the wheel keeps scrolling the panel.
Item {
  id: slider
  property var panel: null
  property real value: 0
  property real minimum: 0
  property real maximum: 1
  property real step: 1
  property bool interactive: true
  property bool dragging: false
  property real liveValue: value
  readonly property real trackHeight: Math.max(4, Math.round(Style.spacing.controlHeight * 0.11))
  readonly property real knobSize: Math.max(14, Math.round(Style.spacing.controlHeight * 0.38))
  readonly property real range: Math.max(0.0001, maximum - minimum)
  readonly property real progress: Math.max(0, Math.min(1, (liveValue - minimum) / range))
  readonly property bool hot: area.containsMouse || dragging
  signal moved(real value)
  signal released(real value)

  function snap(v) {
    var s = step > 0 ? Math.round((v - minimum) / step) * step + minimum : v
    return Math.max(minimum, Math.min(maximum, s))
  }

  onValueChanged: if (!dragging) liveValue = value
  implicitHeight: Math.max(Style.space(22), knobSize + Style.spacing.md)
  opacity: interactive ? 1 : 0.45

  Rectangle {
    id: track
    anchors.verticalCenter: parent.verticalCenter
    anchors.left: parent.left
    anchors.right: parent.right
    height: slider.trackHeight
    radius: height / 2
    color: Style.selectedFillFor(slider.panel.foreground, slider.panel.accent)
  }
  Rectangle {
    anchors.verticalCenter: track.verticalCenter
    anchors.left: track.left
    height: track.height
    radius: track.radius
    color: slider.panel.foreground
    width: track.width * slider.progress
    Behavior on width { enabled: !slider.dragging; NumberAnimation { duration: 140; easing.type: Easing.OutCubic } }
  }
  Rectangle {
    width: slider.knobSize
    height: slider.knobSize
    radius: slider.knobSize / 2
    color: slider.panel.foreground
    border.width: Math.max(1, Style.space(2))
    border.color: slider.panel.bar ? slider.panel.bar.background : Color.background
    anchors.verticalCenter: track.verticalCenter
    x: Math.max(0, Math.min(track.width - width, track.width * slider.progress - width / 2))
    scale: slider.hot ? 1.15 : 1.0
    Behavior on x { enabled: !slider.dragging; NumberAnimation { duration: 140; easing.type: Easing.OutCubic } }
    Behavior on scale { NumberAnimation { duration: 110; easing.type: Easing.OutCubic } }
  }
  MouseArea {
    id: area
    anchors.fill: parent
    enabled: slider.interactive
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    acceptedButtons: Qt.LeftButton
    function valueFromX(x) {
      var clamped = Math.max(0, Math.min(track.width, x))
      return slider.snap(slider.minimum + (clamped / Math.max(1, track.width)) * slider.range)
    }
    onPressed: function (mouse) { slider.dragging = true; slider.liveValue = valueFromX(mouse.x); slider.moved(slider.liveValue) }
    onPositionChanged: function (mouse) { if (slider.dragging) { slider.liveValue = valueFromX(mouse.x); slider.moved(slider.liveValue) } }
    onReleased: function () { slider.dragging = false; slider.released(slider.liveValue) }
    // No onWheel on purpose.
  }
}
```

- [ ] **Step 2: `SliderRow.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui
import "Model.js" as Model

CursorSurface {
  id: row
  property var panel: null
  property string label: ""
  property string badge: ""
  property real value: 0
  property bool known: true
  property real minimum: 0
  property real maximum: 1
  property real step: 1
  property string unit: ""
  property bool logScale: false
  property bool interactive: true
  property var format: null
  signal committed(real value)

  readonly property real sliderMin: logScale ? 0 : minimum
  readonly property real sliderMax: logScale ? 1000 : maximum
  readonly property real sliderValue: logScale ? Model.freqToPos(value, minimum, maximum) : value
  function toValue(p) { return logScale ? Model.posToFreq(p, minimum, maximum) : p }
  function text(v) { return !known && !slider.dragging ? "—" : (format ? format(v) : Model.formatValue(v, unit)) }
  function adjust(dir) {
    if (!interactive) return
    var p = slider.snap(sliderValue + dir * (logScale ? 25 : step))
    committed(toValue(p))
  }
  function activate() {}

  outline: true
  foreground: panel.foreground
  accent: panel.accent
  width: parent ? parent.width : 0
  implicitHeight: col.implicitHeight + Style.spacing.md * 2

  Column {
    id: col
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.leftMargin: Style.spacing.rowPaddingX
    anchors.rightMargin: Style.spacing.rowPaddingX
    spacing: Style.spacing.xs
    Item {
      width: parent.width
      implicitHeight: labelText.implicitHeight
      Text {
        id: labelText
        text: row.label + (row.badge !== "" ? "  ·  " + row.badge : "")
        color: row.interactive ? row.panel.foreground : row.panel.dim
        font.family: row.panel.fontFamily
        font.pixelSize: Style.font.bodySmall
      }
      Text {
        anchors.right: parent.right
        text: row.text(slider.dragging ? row.toValue(slider.liveValue) : row.value)
        color: row.panel.dim
        font.family: row.panel.fontFamily
        font.pixelSize: Style.font.bodySmall
      }
    }
    QuietSlider {
      id: slider
      panel: row.panel
      width: parent.width
      value: row.sliderValue
      minimum: row.sliderMin
      maximum: row.sliderMax
      step: row.logScale ? 1 : row.step
      interactive: row.interactive
      onMoved: function (v) { debounce.pendingValue = row.toValue(v); debounce.restart() }
      onReleased: function (v) { debounce.stop(); row.committed(row.toValue(v)) }
    }
  }
  Timer {
    id: debounce
    property real pendingValue: 0
    interval: 120
    onTriggered: row.committed(pendingValue)
  }
  MouseArea {
    anchors.fill: parent
    acceptedButtons: Qt.NoButton
    hoverEnabled: true
    onEntered: row.panel.hoverRow(row)
  }
}
```

- [ ] **Step 3: `ToggleRow.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui

Toggle {
  id: row
  property var panel: null
  property bool interactive: true
  property string baseLabel: ""
  property string badge: ""
  signal toggledTo(bool value)

  function adjust(dir) { if (interactive) toggledTo(dir > 0) }
  function activate() { if (interactive) toggledTo(!checked) }

  label: baseLabel + (badge !== "" ? "  ·  " + badge : "")
  foreground: panel.foreground
  accent: panel.accent
  fontFamily: panel.fontFamily
  enabled: interactive
  opacity: interactive ? 1 : 0.45
  width: parent ? parent.width : 0
  onClicked: if (interactive) toggledTo(!checked)
  onHovered: function (isHovered) { if (isHovered) row.panel.hoverRow(row) }
}
```

Callers set `baseLabel` (not `label`) so the badge can be appended.

- [ ] **Step 4: `ChipRow.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui

CursorSurface {
  id: row
  property var panel: null
  property string label: ""
  property var options: []
  property var value: null
  property bool multi: false
  property bool swatches: false
  property bool interactive: true
  signal picked(var value)

  function same(a, b) { return JSON.stringify(a) === JSON.stringify(b) }
  function isSelected(v) {
    if (!multi) return same(v, value)
    var list = value || []
    for (var i = 0; i < list.length; i++) if (same(list[i], v)) return true
    return false
  }
  function indexOfValue() { for (var i = 0; i < options.length; i++) if (isSelected(options[i].value)) return i; return -1 }
  function adjust(dir) {
    if (!interactive || options.length === 0 || multi) return
    var i = indexOfValue()
    var n = i < 0 ? (dir > 0 ? 0 : options.length - 1) : Math.max(0, Math.min(options.length - 1, i + dir))
    if (n !== i) picked(options[n].value)
  }
  function activate() {}

  outline: true
  foreground: panel.foreground
  accent: panel.accent
  width: parent ? parent.width : 0
  implicitHeight: col.implicitHeight + Style.spacing.md * 2
  opacity: interactive ? 1 : 0.45

  Column {
    id: col
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.leftMargin: Style.spacing.rowPaddingX
    anchors.rightMargin: Style.spacing.rowPaddingX
    spacing: Style.spacing.xs
    Text {
      visible: row.label !== ""
      text: row.label
      color: row.panel.foreground
      font.family: row.panel.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
    Flow {
      width: parent.width
      spacing: Style.spacing.sm
      Repeater {
        model: row.options
        Button {
          text: row.swatches ? "" : modelData.label
          tooltipText: row.swatches ? modelData.label : ""
          selected: row.isSelected(modelData.value)
          bordered: true
          enabled: row.interactive
          foreground: row.panel.foreground
          accent: row.panel.accent
          fontFamily: row.panel.fontFamily
          fontSize: Style.font.bodySmall
          width: row.swatches ? Style.space(30) : implicitWidth
          onClicked: row.picked(modelData.value)
          onHovered: function (isHovered) { if (isHovered) row.panel.hoverRow(row) }
          Rectangle {
            visible: row.swatches
            anchors.centerIn: parent
            width: Style.space(14)
            height: width
            radius: width / 2
            color: modelData.color || "transparent"
          }
        }
      }
    }
  }
}
```

- [ ] **Step 5: `Fold.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui
import "Model.js" as Model

Column {
  id: fold
  property var panel: null
  property string title: ""
  property string badge: ""
  property bool open: false
  property bool hasCursor: false
  default property alias body: inner.data

  function adjust(dir) { open = dir > 0 }
  function activate() { open = !open }

  width: parent ? parent.width : 0
  spacing: Style.spacing.xs

  CursorSurface {
    hasCursor: fold.hasCursor
    outline: true
    foreground: fold.panel.foreground
    accent: fold.panel.accent
    width: parent.width
    implicitHeight: head.implicitHeight + Style.spacing.md * 2
    Row {
      id: head
      anchors.left: parent.left
      anchors.leftMargin: Style.spacing.rowPaddingX
      anchors.verticalCenter: parent.verticalCenter
      spacing: Style.spacing.sm
      Text { text: Model.glyph(fold.open ? "unfold" : "fold"); color: fold.panel.dim; font.family: fold.panel.fontFamily; font.pixelSize: Style.font.bodySmall }
      Text { text: fold.title; color: fold.panel.foreground; font.family: fold.panel.fontFamily; font.pixelSize: Style.font.bodySmall; font.bold: true }
      Text { visible: fold.badge !== ""; text: fold.badge; color: fold.panel.accent; font.family: fold.panel.fontFamily; font.pixelSize: Style.font.caption }
    }
    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: Qt.PointingHandCursor
      onClicked: fold.open = !fold.open
      onEntered: fold.panel.hoverRow(fold)
    }
  }
  Column {
    id: inner
    visible: fold.open
    width: parent.width
    leftPadding: Style.spacing.lg
    spacing: Style.spacing.xs
  }
}
```

- [ ] **Step 6: `TabStrip.qml`** (the Sony tab strip as a row with a cursor)

```qml
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import qs.Commons
import qs.Ui

CursorSurface {
  id: strip
  property var panel: null
  property var tabs: []
  property string current: ""
  signal picked(string key)

  function adjust(dir) {
    var i = 0
    for (var k = 0; k < tabs.length; k++) if (tabs[k].key === current) i = k
    var n = Math.max(0, Math.min(tabs.length - 1, i + dir))
    if (n !== i) picked(tabs[n].key)
  }
  function activate() {}

  outline: true
  foreground: panel.foreground
  accent: panel.accent
  width: parent ? parent.width : 0
  implicitHeight: tabRow.implicitHeight + Style.space(6)

  RowLayout {
    id: tabRow
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.top: parent.top
    spacing: Style.spacing.xs
    Repeater {
      model: strip.tabs
      Item {
        id: tab
        readonly property bool active: modelData.key === strip.current
        readonly property bool hot: tabMouse.containsMouse
        Layout.fillWidth: true
        implicitHeight: tabLabel.implicitHeight + Style.spacing.controlPaddingY * 2 + Style.space(4)
        Rectangle {
          anchors.fill: parent
          anchors.bottomMargin: Style.space(4)
          radius: Style.cornerRadius
          color: tab.hot ? Style.hoverFillFor(strip.panel.foreground, strip.panel.accent) : "transparent"
          Behavior on color { ColorAnimation { duration: 100 } }
        }
        Column {
          id: tabLabel
          anchors.centerIn: parent
          anchors.verticalCenterOffset: -Style.space(2)
          spacing: 0
          Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: modelData.glyph
            color: tab.active ? strip.panel.foreground : strip.panel.dim
            font.family: strip.panel.fontFamily
            font.pixelSize: Style.font.icon
          }
          Text {
            anchors.horizontalCenter: parent.horizontalCenter
            text: modelData.label
            color: tab.active ? strip.panel.foreground : strip.panel.dim
            font.family: strip.panel.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: tab.active
          }
        }
        Rectangle {
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.bottom: parent.bottom
          height: Style.space(2)
          radius: height / 2
          color: tab.active ? strip.panel.accent : Style.normalFillFor(strip.panel.foreground, strip.panel.accent)
          Behavior on color { ColorAnimation { duration: 140 } }
        }
        MouseArea {
          id: tabMouse
          anchors.fill: parent
          hoverEnabled: true
          cursorShape: Qt.PointingHandCursor
          onEntered: strip.panel.hoverRow(strip)
          onClicked: strip.picked(modelData.key)
        }
      }
    }
  }
}
```

- [ ] **Step 7: `NameRow.qml`, `ActionRow.qml`, `HintText.qml`**

`NameRow.qml`:

```qml
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import qs.Commons
import qs.Ui

CursorSurface {
  id: row
  property var panel: null
  property string placeholder: ""
  property string buttonText: ""
  property string text: ""
  signal submitted(string text)

  function activate() { field.forceActiveFocus() }
  function adjust(dir) {}
  function submit() {
    var t = field.text.trim()
    if (t === "") return
    row.submitted(t)
    field.text = row.text
    field.focus = false
  }

  outline: true
  foreground: panel.foreground
  accent: panel.accent
  width: parent ? parent.width : 0
  implicitHeight: line.implicitHeight + Style.spacing.md * 2

  RowLayout {
    id: line
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.leftMargin: Style.spacing.rowPaddingX
    anchors.rightMargin: Style.spacing.rowPaddingX
    spacing: Style.spacing.sm
    TextField {
      id: field
      Layout.fillWidth: true
      placeholderText: row.placeholder
      text: row.text
      foreground: row.panel.foreground
      accent: row.panel.accent
      hasCursor: row.hasCursor
      onActiveFocusChanged: {
        row.panel.editing = activeFocus
        if (!activeFocus) Qt.callLater(row.panel.refocus)
      }
      onAccepted: row.submit()
      Keys.onPressed: function (e) { if (e.key === Qt.Key_Escape) { field.text = row.text; field.focus = false; e.accepted = true } }
    }
    Button {
      text: row.buttonText
      bordered: true
      enabled: field.text.trim() !== ""
      foreground: row.panel.foreground
      accent: row.panel.accent
      fontFamily: row.panel.fontFamily
      fontSize: Style.font.bodySmall
      onClicked: row.submit()
    }
  }
}
```

`ActionRow.qml`:

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui

CursorSurface {
  id: row
  property var panel: null
  property string text: ""
  property bool interactive: true
  property bool destructive: false
  signal triggered()

  function activate() { if (interactive) triggered() }
  function adjust(dir) {}

  outline: true
  foreground: panel.foreground
  accent: panel.accent
  width: parent ? parent.width : 0
  implicitHeight: btn.implicitHeight + Style.spacing.xs * 2

  Button {
    id: btn
    anchors.left: parent.left
    anchors.leftMargin: Style.spacing.rowPaddingX
    anchors.verticalCenter: parent.verticalCenter
    text: row.text
    bordered: true
    enabled: row.interactive
    foreground: row.destructive ? row.panel.urgent : row.panel.foreground
    accent: row.panel.accent
    fontFamily: row.panel.fontFamily
    fontSize: Style.font.bodySmall
    onClicked: row.triggered()
    onHovered: function (isHovered) { if (isHovered) row.panel.hoverRow(row) }
  }
}
```

`HintText.qml`:

```qml
import QtQuick
import qs.Commons

Text {
  property var panel: null
  width: parent ? parent.width : 0
  leftPadding: Style.spacing.rowPaddingX
  rightPadding: Style.spacing.rowPaddingX
  wrapMode: Text.WordWrap
  color: panel.dim
  font.family: panel.fontFamily
  font.pixelSize: Style.font.caption
}
```

- [ ] **Step 8: `EqCurve.qml`**

```qml
import QtQuick
import qs.Commons
import "Model.js" as Model

// The real response of the cleanup chain (HPF, 5 bands, LPF), ±18 dB, log 20 Hz–20 kHz.
Item {
  id: curve
  property var panel: null
  property var filter: null
  implicitHeight: Style.space(72)
  onFilterChanged: canvas.requestPaint()
  Connections { target: curve.panel; function onForegroundChanged() { canvas.requestPaint() } }

  Canvas {
    id: canvas
    anchors.fill: parent
    onPaint: {
      var ctx = getContext("2d")
      ctx.reset()
      var w = width, h = height, pad = Style.space(6), range = 18
      var fg = curve.panel.foreground, acc = curve.panel.accent
      function yOf(db) { return pad + (0.5 - Math.max(-range, Math.min(range, db)) / (2 * range)) * (h - 2 * pad) }
      function xOf(f) { return pad + Math.log(f / 20) / Math.log(1000) * (w - 2 * pad) }
      ctx.strokeStyle = Style.normalFillFor(fg, acc)
      ctx.lineWidth = 1
      ctx.beginPath(); ctx.moveTo(pad, yOf(0)); ctx.lineTo(w - pad, yOf(0)); ctx.stroke()
      var grid = [100, 1000, 10000]
      for (var g = 0; g < grid.length; g++) { ctx.beginPath(); ctx.moveTo(xOf(grid[g]), pad); ctx.lineTo(xOf(grid[g]), h - pad); ctx.stroke() }
      if (!curve.filter) return
      var pts = Model.chainResponse(curve.filter, 96)
      ctx.beginPath()
      for (var i = 0; i < pts.length; i++) {
        var x = pad + pts[i].x * (w - 2 * pad), y = yOf(pts[i].db)
        if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y)
      }
      ctx.strokeStyle = acc
      ctx.lineWidth = Math.max(1.5, Style.space(2))
      ctx.lineJoin = "round"
      ctx.stroke()
    }
  }
}
```

- [ ] **Step 9: Service-bound rows**

`MicSliderRow.qml`:

```qml
import QtQuick
import "Model.js" as Model

SliderRow {
  id: row
  property string key: ""
  readonly property var svc: panel ? panel.service : null
  readonly property var schema: panel ? panel.schema : null
  readonly property var raw: svc ? svc.value(key) : null
  label: panel ? Model.fieldLabel(panel.tr, key) : key
  minimum: Model.fieldMin(schema, key, 0)
  maximum: Model.fieldMax(schema, key, 1)
  step: Model.fieldStep(schema, key, 1)
  unit: Model.fieldUnit(schema, key)
  known: typeof raw === "number"
  value: known ? raw : minimum
  interactive: !!panel && panel.connected
  onCommitted: function (v) { var c = {}; c[row.key] = row.step >= 1 ? Math.round(v) : v; row.svc.setMic(c) }
}
```

`MicToggleRow.qml`:

```qml
import QtQuick
import "Model.js" as Model

ToggleRow {
  id: row
  property string key: ""
  readonly property var svc: panel ? panel.service : null
  baseLabel: panel ? Model.fieldLabel(panel.tr, key) : key
  checked: !!svc && svc.value(key) === true
  interactive: !!panel && panel.connected
  onToggledTo: function (v) { var c = {}; c[row.key] = v; row.svc.setMic(c) }
}
```

`MicChipRow.qml`:

```qml
import QtQuick
import "Model.js" as Model

ChipRow {
  id: row
  property string key: ""
  readonly property var svc: panel ? panel.service : null
  label: panel ? Model.fieldLabel(panel.tr, key) : key
  options: panel ? Model.enumOptions(panel.schema, key, panel.tr) : []
  value: svc ? svc.value(key) : null
  interactive: !!panel && panel.connected
  onPicked: function (v) { var c = {}; c[row.key] = v; row.svc.setMic(c) }
}
```

`FilterSliderRow.qml`:

```qml
import QtQuick
import "Model.js" as Model

SliderRow {
  id: row
  property string path: ""
  readonly property var svc: panel ? panel.service : null
  readonly property var schema: panel ? panel.schema : null
  readonly property var raw: svc ? svc.filterValue(path) : null
  minimum: Model.filterMin(schema, path, 0)
  maximum: Model.filterMax(schema, path, 1)
  step: Model.filterStep(schema, path, 1)
  unit: Model.filterUnit(schema, path)
  known: typeof raw === "number"
  value: known ? raw : minimum
  interactive: !!panel && panel.status.ready
  onCommitted: function (v) { var c = {}; c[row.path] = v; row.svc.setFilter(c) }
}
```

`FilterToggleRow.qml`:

```qml
import QtQuick

ToggleRow {
  id: row
  property string path: ""
  readonly property var svc: panel ? panel.service : null
  checked: !!svc && svc.filterValue(path) === true
  interactive: !!panel && panel.status.ready
  onToggledTo: function (v) { var c = {}; c[row.path] = v; row.svc.setFilter(c) }
}
```

`FieldRows.qml` (renders descriptor fields generically, used only for the experimental DSP):

```qml
import QtQuick
import qs.Commons
import "Model.js" as Model

Column {
  id: list
  property var panel: null
  property var keys: []
  function rowItems() {
    var out = []
    for (var i = 0; i < rep.count; i++) { var l = rep.itemAt(i); if (l && l.item) out.push(l.item) }
    return out
  }
  width: parent ? parent.width : 0
  spacing: Style.spacing.xs

  Repeater {
    id: rep
    model: list.keys
    Loader {
      id: slot
      width: list.width
      readonly property string key: modelData
      readonly property var spec: Model.field(list.panel.schema, modelData)
      sourceComponent: !spec ? null : spec.type === "bool" ? toggleC : spec.type === "enum" ? chipC : sliderC
      Component { id: toggleC; MicToggleRow { panel: list.panel; key: slot.key; badge: list.panel.tr("badge.unverified") } }
      Component { id: chipC; MicChipRow { panel: list.panel; key: slot.key } }
      Component { id: sliderC; MicSliderRow { panel: list.panel; key: slot.key; badge: list.panel.tr("badge.unverified") } }
    }
  }
}
```

- [ ] **Step 10: Syntax check and commit**

Run: `for f in shell/*.qml; do /usr/lib/qt6/bin/qmllint "$f" 2>&1 | grep '\[syntax\]' && echo "SYNTAX ERROR in $f"; done && node --test shell/tests/`
Expected: no syntax output; node tests all pass (strings coverage included).

```bash
git add shell/QuietSlider.qml shell/SliderRow.qml shell/ToggleRow.qml shell/ChipRow.qml shell/Fold.qml shell/TabStrip.qml shell/NameRow.qml shell/ActionRow.qml shell/HintText.qml shell/EqCurve.qml shell/MicSliderRow.qml shell/MicToggleRow.qml shell/MicChipRow.qml shell/FilterSliderRow.qml shell/FilterToggleRow.qml shell/FieldRows.qml
git commit -m "feat(panel): row controls with a shared cursor contract, EQ curve, service-bound rows"
```

---

### Task 6: `manifest.json` and `BarWidget.qml`

**Files:**
- Replace: `shell/manifest.json` (the upstream manifest for id `maono`)
- Create: `shell/BarWidget.qml`
- Delete: `shell/Panel.qml` from upstream. Task 7 writes the new one; delete the old file in this task so nothing references it.

**Interfaces:**
- The bar widget's root exposes `opened`, `open()`, `close()`, `toggle()`, `closeForPopoutSwitch()` and `popoutSwitchClosing`. This is the shape `omarchy-shell shell toggle <id>` needs.
- It forwards `bar`, `settings`, `anchorItem` and `hostWidget` to `Panel.qml`, and pushes `settings` into the service.
- UI preferences, declared in `barWidget.schema` with defaults:

| Key | Type | Values | Default |
|---|---|---|---|
| `language` | enum | `auto` / `es` / `en` | `auto` |
| `experimental` | boolean | | false |
| `middleClick` | enum | `mute` / `nextProfile` / `live` / `none` | `mute` |
| `rightClick` | enum | same as `middleClick` | `nextProfile` |
| `wheelStep` | integer | 1–5 | 1 |
| `hideWhenDisconnected` | boolean | | false |
| `showBattery` | boolean | | true |
| `binary` | string | | `maono` |

- [ ] **Step 1: `shell/manifest.json`**

```json
{
  "schemaVersion": 1,
  "id": "io.github.agusmoura.maono",
  "name": "Maono Microphone",
  "version": "0.1.0",
  "author": "Agus (backend protocol by MD Shahriyar Alam)",
  "license": "MIT",
  "description": "Maono PD100W control: gain, noise reduction, PipeWire EQ/RNNoise/compressor, light, profiles and a live monitor, driven by `maono serve`.",
  "kinds": ["service", "bar-widget"],
  "entryPoints": { "service": "Service.qml", "barWidget": "BarWidget.qml" },
  "barWidget": {
    "displayName": "Maono Microphone",
    "description": "Mute, gain, cleanup filter, light, profiles and live monitor for the Maono PD100W.",
    "category": "Audio",
    "allowMultiple": false,
    "defaultSection": "right",
    "defaults": {
      "language": "auto", "experimental": false, "middleClick": "mute", "rightClick": "nextProfile",
      "wheelStep": 1, "hideWhenDisconnected": false, "showBattery": true, "binary": "maono"
    },
    "schema": [
      { "key": "language", "type": "enum", "label": "Language", "options": ["auto", "es", "en"], "defaultValue": "auto" },
      { "key": "experimental", "type": "boolean", "label": "Show experimental mic DSP", "defaultValue": false },
      { "key": "middleClick", "type": "enum", "label": "Middle click", "options": ["mute", "nextProfile", "live", "none"], "defaultValue": "mute" },
      { "key": "rightClick", "type": "enum", "label": "Right click", "options": ["mute", "nextProfile", "live", "none"], "defaultValue": "nextProfile" },
      { "key": "wheelStep", "type": "integer", "label": "Gain change per wheel notch", "min": 1, "max": 5, "step": 1, "defaultValue": 1 },
      { "key": "hideWhenDisconnected", "type": "boolean", "label": "Hide when the mic is disconnected", "defaultValue": false },
      { "key": "showBattery", "type": "boolean", "label": "Low-battery dot on the icon", "defaultValue": true },
      { "key": "binary", "type": "string", "label": "maono binary", "description": "Name on PATH or absolute path of the maono CLI.", "defaultValue": "maono" }
    ]
  }
}
```

- [ ] **Step 2: `shell/BarWidget.qml`**

```qml
import QtQuick
import qs.Commons
import qs.Ui
import "Model.js" as Model

BarWidget {
  id: root
  moduleName: "io.github.agusmoura.maono"
  readonly property string pluginId: "io.github.agusmoura.maono"

  property int _svcTick: 0
  readonly property var service: { root._svcTick; return root.bar && root.bar.shell ? root.bar.shell.serviceFor(root.pluginId) : null }
  Timer { interval: 200; repeat: true; running: root.service === null && root.bar !== null; onTriggered: root._svcTick++ }

  readonly property var manifest: service ? service.manifest : null
  function pref(key, fallback) { return root.setting(key, Model.prefDefault(root.manifest, key, fallback)) }

  property real wheelAccumulator: 0
  readonly property bool connected: !!service && service.connected
  readonly property bool muted: !!service && service.value("mic.mute") === true
  readonly property var battery: service ? service.value("info.battery") : null

  // Shape contract for shell.summon/hide/toggle (Bar.findPanelWidget).
  readonly property bool opened: panelLoader.item ? panelLoader.item.opened === true : false
  readonly property bool popoutSwitchClosing: panelLoader.item ? panelLoader.item.popoutSwitchClosing === true : false
  function open() { if (panelLoader.item) panelLoader.item.open() }
  function close() { if (panelLoader.item) panelLoader.item.close() }
  function toggle() { if (panelLoader.item) panelLoader.item.toggle() }
  function closeForPopoutSwitch() { if (panelLoader.item) panelLoader.item.closeForPopoutSwitch() }

  function injectPanel() {
    var p = panelLoader.item
    if (!p) return
    p.bar = root.bar
    p.settings = root.settings
    p.anchorItem = button
    p.hostWidget = root
  }
  function pushSettings() { if (root.service) root.service.settings = root.settings }
  function runAction(name) {
    if (!root.service) return
    if (name === "mute") root.service.toggleMute()
    else if (name === "nextProfile") root.service.nextProfile()
    else if (name === "live") root.service.toggleLive()
  }

  visible: !(root.pref("hideWhenDisconnected", false) === true && !root.connected)
  implicitWidth: visible ? button.implicitWidth : 0
  implicitHeight: button.implicitHeight
  onBarChanged: injectPanel()
  onSettingsChanged: { injectPanel(); pushSettings() }
  onServiceChanged: pushSettings()

  Loader {
    id: panelLoader
    active: true
    source: Qt.resolvedUrl("Panel.qml")
    visible: false
    onLoaded: { root.injectPanel(); Qt.callLater(root.injectPanel) }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: Model.glyph(root.muted ? "micOff" : "mic")
    active: root.muted
    dimmed: !root.connected
    tooltipText: root.service ? Model.barTooltip(root.service.tr, root.service.st, root.service.status.profiles) : root.pluginId
    onPressed: function (b) {
      if (b === Qt.LeftButton) root.toggle()
      else if (b === Qt.MiddleButton) root.runAction(root.pref("middleClick", "mute"))
      else if (b === Qt.RightButton) root.runAction(root.pref("rightClick", "nextProfile"))
    }
    onWheelMoved: function (delta) {
      var w = Util.wheelSteps(root.wheelAccumulator, delta)
      root.wheelAccumulator = w.remainder
      if (w.steps === 0 || !root.service || !root.connected) return
      var g = root.service.value("mic.gain")
      if (typeof g !== "number") return
      var schema = root.service.status.schema
      var step = Number(root.pref("wheelStep", 1)) || 1
      var next = Math.max(Model.fieldMin(schema, "mic.gain", 0), Math.min(Model.fieldMax(schema, "mic.gain", 20), g + w.steps * step))
      if (next !== g) root.service.setMic({ "mic.gain": next })
    }

    Rectangle {
      visible: root.pref("showBattery", true) === true && typeof root.battery === "number" && root.battery <= 20
      width: Style.space(5)
      height: width
      radius: width / 2
      color: root.bar ? root.bar.urgent : Color.urgent
      anchors.right: parent.right
      anchors.top: parent.top
      anchors.margins: Style.space(3)
    }
  }
}
```

The low-battery threshold (20 %) is the one fixed number here. It mirrors the mic's own LED rule ("≤20 %"); add a `lowBattery` preference if it ever needs tuning.

- [ ] **Step 3: Syntax check, validate, commit**

Run:

```bash
git rm -q shell/Panel.qml
/usr/lib/qt6/bin/qmllint shell/BarWidget.qml 2>&1 | grep '\[syntax\]'
python3 -c "import json;json.load(open('shell/manifest.json'))" && echo manifest-ok
```

Expected: no syntax output, then `manifest-ok`. (`omarchy plugin validate` needs `Panel.qml` from Task 7 only indirectly. It checks the entry points, `Service.qml` and `BarWidget.qml`, which both exist. Run it now: `omarchy plugin validate shell` → OK.)

```bash
git add shell/manifest.json shell/BarWidget.qml
git commit -m "feat(panel): manifest as service + bar widget; bar icon with configurable actions"
```

---

### Task 7: `Panel.qml` (hero, live, profile chips, tabs, cursor model)

**Files:**
- Create: `shell/Panel.qml`

**Interfaces (what tabs use from `panel`):**
- Values: `service`, `status`, `schema`, `st`, `connected`, `experimental`, `foreground`, `dim`, `accent`, `urgent`, `fontFamily`, `bar`, `editing` (rw)
- Functions:
  - `tr(key, params)`, `setting(key, fallback)`, `pref(key, fallback)`, `persist(values)`
  - `hoverRow(item)`, `refocus()`, `selectTab(key)`
- Each tab is a `Column` with `property var panel` and `readonly property var rows`. The panel creates it with `setSource(url, {panel: root})`.

- [ ] **Step 1: Write `shell/Panel.qml`**

```qml
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Services.Pipewire
import qs.Commons
import qs.Ui
import "Model.js" as Model

Panel {
  id: root
  moduleName: "io.github.agusmoura.maono"
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null
  readonly property var barIdentity: hostWidget || root

  property int _svcTick: 0
  readonly property var service: { root._svcTick; return root.bar && root.bar.shell ? root.bar.shell.serviceFor("io.github.agusmoura.maono") : null }
  Timer { interval: 200; repeat: true; running: root.service === null && root.bar !== null; onTriggered: root._svcTick++ }

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(foreground, 1.4)
  readonly property color accent: Color.accent
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  readonly property var status: service ? service.status : Model.emptyStatus()
  readonly property var schema: status.schema
  readonly property var st: status.st
  readonly property bool connected: !!service && service.connected
  readonly property var manifest: service ? service.manifest : null
  readonly property bool experimental: pref("experimental", false) === true
  readonly property string bannerKey: Model.banner(!!service, status)
  readonly property bool muted: !!service && service.value("mic.mute") === true
  property bool editing: false

  function tr(key, params) { return service ? service.tr(key, params) : key }
  function pref(key, fallback) { return root.setting(key, Model.prefDefault(root.manifest, key, fallback)) }
  function switchPanel(direction) {
    if (bar && typeof bar.switchPanelFrom === "function") return bar.switchPanelFrom(root.barIdentity, direction)
    return false
  }
  function refocus() { if (root.opened) keyCatcher.forceActiveFocus() }
  function persist(values) {
    var entry = { id: root.moduleName }
    for (var k in root.settings) if (k !== "id") entry[k] = root.settings[k]
    for (var v in values) entry[v] = values[v]
    root.settings = entry
    if (root.hostWidget && "settings" in root.hostWidget) root.hostWidget.settings = entry
    if (root.bar && root.bar.shell && typeof root.bar.shell.updateEntryInline === "function") root.bar.shell.updateEntryInline(root.moduleName, entry)
  }

  // ---- hero data
  readonly property string heroMeta: {
    if (!st || st.device !== "connected") return ""
    var m = st.mic || {}, parts = []
    if (typeof m["info.battery"] === "number") parts.push(tr("hero.battery", { pct: m["info.battery"] }) + (m["info.charging"] === true ? " · " + tr("hero.charging") : ""))
    if (m["info.firmware"]) parts.push(tr("hero.firmware", { v: m["info.firmware"] }))
    return parts.join(" · ")
  }
  readonly property string noticeText: {
    var n = status.notice
    if (!n) return ""
    var base = tr(n.key, n.params || {})
    return n.text ? base + ": " + n.text : base
  }
  readonly property string partialGroups: st && st.partial && st.partial.failed ? st.partial.failed.map(function (f) { return f.group }).join(", ") : ""
  function reapply() { if (service && st && st.activeProfile) service.applyProfile(st.activeProfile) }
  function saveActive() {
    if (!service || !st || !st.activeProfile) return
    service.request({ cmd: "profile.save", name: Model.profileName(status.profiles, st.activeProfile), overwrite: st.activeProfile }, service.report)
  }

  // ---- live monitor
  readonly property bool liveOn: !!st && !!st.monitor && st.monitor.on === true
  readonly property string liveSource: st && st.monitor && st.monitor.source ? st.monitor.source : "clean"
  property bool liveAskForce: false
  function liveResult(ack) {
    var r = Model.liveRefusal(ack)
    root.liveAskForce = r === "force"
    if (r === "error") service.report(ack)
  }
  function toggleLive(force) {
    if (!service) return
    if (liveOn) { service.setMonitor(false, null, false); root.liveAskForce = false; return }
    service.setMonitor(true, null, force, root.liveResult)
  }
  function setLiveSource(src) { if (service) service.setMonitor(liveOn, src, false, root.liveResult) }

  // ---- level meter (what the default input sends)
  readonly property var source: Pipewire.defaultAudioSource
  readonly property string sourceLabel: source ? String(source.description || source.nickname || source.name) : "—"
  property bool clipping: false
  PwNodePeakMonitor { id: meter; node: root.source; enabled: root.opened && !!root.source }
  Connections { target: meter; function onPeakChanged() { if (meter.peak >= 0.98) { root.clipping = true; clipHold.restart() } } }
  Timer { id: clipHold; interval: 1500; onTriggered: root.clipping = false }

  // ---- tabs
  readonly property var tabs: [
    { key: "voice", file: "VoiceTab.qml" }, { key: "eq", file: "EqTab.qml" }, { key: "filter", file: "FilterTab.qml" },
    { key: "light", file: "LightTab.qml" }, { key: "profiles", file: "ProfilesTab.qml" }, { key: "settings", file: "SettingsTab.qml" }
  ]
  property string currentTab: "voice"
  function tabFile(key) { for (var i = 0; i < tabs.length; i++) if (tabs[i].key === key) return tabs[i].file; return tabs[0].file }
  function loadTab() { tabBody.setSource(Qt.resolvedUrl(root.tabFile(root.currentTab)), { panel: root }) }
  function selectTab(key) {
    if (root.currentTab === key) return
    root.currentTab = key
    root.cursor = 1
    root.loadTab()
  }
  Component.onCompleted: loadTab()
  Connections {
    target: root.service
    ignoreUnknownSignals: true
    function onTabRequestSerialChanged() {
      var i = root.service.requestedTab - 1
      if (i >= 0 && i < root.tabs.length) root.selectTab(root.tabs[i].key)
    }
  }

  // ---- one cursor for mouse and keyboard; rows expose hasCursor/adjust()/activate()[/remove()]
  property int cursor: 0
  property bool cursorActive: false
  readonly property var rows: {
    var r = [profileRow, tabStrip]
    var item = tabBody.item
    if (item && item.rows) r = r.concat(item.rows)
    return r
  }
  onRowsChanged: refreshCursor(false)
  onOpenedChanged: if (opened) { root.cursorActive = false; root.cursor = 0; root.editing = false; root.liveAskForce = false; root.refreshCursor(false) }
  function refreshCursor(scroll) {
    if (rows.length === 0) return
    root.cursor = Math.max(0, Math.min(root.cursor, rows.length - 1))
    for (var i = 0; i < rows.length; i++) if (rows[i]) rows[i].hasCursor = root.cursorActive && i === root.cursor
    if (scroll && root.cursorActive) root.ensureVisible(rows[root.cursor])
  }
  function move(dy) {
    root.cursorActive = true
    root.cursor = Math.max(0, Math.min(rows.length - 1, root.cursor + dy))
    root.refreshCursor(true)
  }
  function hoverRow(item) {
    var i = rows.indexOf(item)
    if (i < 0) return
    root.cursor = i
    root.cursorActive = true
    root.refreshCursor(false)
  }
  function current() { return root.cursorActive ? rows[root.cursor] : null }
  function ensureVisible(item) {
    if (!item || !scrollArea.contentItem) return
    var p = item.mapToItem(content, 0, 0)
    var f = scrollArea.contentItem
    if (p.y < f.contentY) f.contentY = p.y
    else if (p.y + item.height > f.contentY + scrollArea.height) f.contentY = p.y + item.height - scrollArea.height
  }

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.barIdentity
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(420))
    contentHeight: panel.fittedContentHeight(content.implicitHeight, Style.space(640))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      blocked: root.editing
      onCloseRequested: root.close()
      onTabRequested: function (direction) { root.switchPanel(direction) }
      onMoveRequested: function (dx, dy) {
        if (dy !== 0) { root.move(dy); return }
        var r = root.current()
        if (r && typeof r.adjust === "function") r.adjust(dx)
        else root.move(0)
      }
      onActivateRequested: { var r = root.current(); if (r && typeof r.activate === "function") r.activate() }
      onDeleteRequested: { var r = root.current(); if (r && typeof r.remove === "function") r.remove() }
      onTextKey: function (text) {
        var n = parseInt(text, 10)
        if (n >= 1 && n <= root.tabs.length) root.selectTab(root.tabs[n - 1].key)
        else if (text === "m") { if (root.service) root.service.toggleMute() }
        else if (text === "v") root.toggleLive(false)
        else if (text === "r") root.reapply()
      }

      ScrollView {
        id: scrollArea
        anchors.fill: parent
        clip: true
        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
        ScrollBar.vertical.policy: content.implicitHeight > height ? ScrollBar.AsNeeded : ScrollBar.AlwaysOff
        Binding { target: scrollArea.contentItem; property: "interactive"; value: content.implicitHeight > scrollArea.height }

        Column {
          id: content
          width: scrollArea.availableWidth
          spacing: Style.spacing.md

          PanelHero {
            width: parent.width
            title: root.tr("hero.title")
            meta: root.heroMeta
            foreground: root.foreground
            fontFamily: root.fontFamily
            iconComponent: Component {
              Text {
                text: Model.glyph(root.muted ? "micOff" : "mic")
                color: root.muted ? root.urgent : root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.display
              }
            }
            trailingControl: Component {
              ToggleSwitch {
                checked: root.muted
                interactive: root.connected
                foreground: root.foreground
                accent: root.urgent
                onToggled: if (root.service) root.service.toggleMute()
              }
            }
          }

          Text {
            visible: root.bannerKey !== ""
            width: parent.width
            wrapMode: Text.WordWrap
            text: Model.glyph("warn") + "  " + root.tr(root.bannerKey)
            color: root.bannerKey === "banner.connecting" ? root.dim : root.urgent
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }

          Column {
            visible: root.noticeText !== ""
            width: parent.width
            spacing: Style.spacing.xs
            Text {
              width: parent.width
              wrapMode: Text.WordWrap
              text: root.noticeText
              color: root.status.notice && root.status.notice.kind === "error" ? root.urgent : root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
            Row {
              spacing: Style.spacing.sm
              Button {
                visible: !!root.status.notice && root.status.notice.kind === "recover"
                text: root.tr("notice.recover")
                bordered: true
                foreground: root.foreground; accent: root.accent; fontFamily: root.fontFamily; fontSize: Style.font.bodySmall
                onClicked: root.service.request({ cmd: "recover" }, function (ack) { root.service.report(ack); if (ack.ok) root.service.dismissNotice() })
              }
              Button {
                text: root.tr("notice.dismiss")
                bordered: true
                foreground: root.foreground; accent: root.accent; fontFamily: root.fontFamily; fontSize: Style.font.bodySmall
                onClicked: root.service.dismissNotice()
              }
            }
          }

          Column {
            width: parent.width
            spacing: Style.spacing.xs
            Item {
              width: parent.width
              implicitHeight: meterLabel.implicitHeight
              Text {
                id: meterLabel
                text: root.tr("hero.meter", { source: root.sourceLabel })
                color: root.dim
                elide: Text.ElideRight
                width: parent.width - clipText.width - Style.spacing.sm
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
              Text {
                id: clipText
                anchors.right: parent.right
                visible: root.clipping
                text: root.tr("hero.clip")
                color: root.urgent
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
              }
            }
            Rectangle {
              width: parent.width
              height: Math.max(Style.space(5), Style.spacing.xs)
              radius: height / 2
              color: Util.alpha(root.foreground, 0.18)
              Rectangle {
                height: parent.height
                radius: parent.radius
                width: parent.width * Math.max(0, Math.min(1, meter.peak))
                color: root.clipping ? root.urgent : root.foreground
                Behavior on width { NumberAnimation { duration: 70 } }
              }
            }
          }

          Flow {
            width: parent.width
            spacing: Style.spacing.sm
            Button {
              text: root.tr("hero.live")
              iconText: Model.glyph("live")
              selected: root.liveOn
              bordered: true
              enabled: !!root.service && root.status.ready
              foreground: root.foreground; accent: root.accent; fontFamily: root.fontFamily; fontSize: Style.font.bodySmall
              onClicked: root.toggleLive(false)
            }
            Button {
              text: root.tr("hero.liveClean")
              selected: root.liveSource === "clean"
              bordered: true
              enabled: !!root.service && root.status.ready
              foreground: root.foreground; accent: root.accent; fontFamily: root.fontFamily; fontSize: Style.font.bodySmall
              onClicked: root.setLiveSource("clean")
            }
            Button {
              text: root.tr("hero.liveRaw")
              selected: root.liveSource === "raw"
              bordered: true
              enabled: !!root.service && root.status.ready
              foreground: root.foreground; accent: root.accent; fontFamily: root.fontFamily; fontSize: Style.font.bodySmall
              onClicked: root.setLiveSource("raw")
            }
          }
          Column {
            visible: root.liveAskForce
            width: parent.width
            spacing: Style.spacing.xs
            Text {
              width: parent.width
              wrapMode: Text.WordWrap
              text: root.tr("live.notHeadphones")
              color: root.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
            }
            Button {
              text: root.tr("live.force")
              bordered: true
              foreground: root.urgent; accent: root.accent; fontFamily: root.fontFamily; fontSize: Style.font.bodySmall
              onClicked: root.toggleLive(true)
            }
          }

          ChipRow {
            id: profileRow
            panel: root
            label: root.tr("hero.profile")
            options: root.status.profiles.map(function (p) { return { value: p.id, label: p.name } })
            value: root.st ? root.st.activeProfile : null
            interactive: !!root.service && root.status.ready
            onPicked: function (v) { root.service.applyProfile(v) }
          }
          Row {
            visible: !!root.st && (root.st.dirty === true || !!root.st.partial)
            spacing: Style.spacing.sm
            Text {
              anchors.verticalCenter: parent.verticalCenter
              text: root.st && root.st.partial ? root.tr("hero.partial", { groups: root.partialGroups }) : root.tr("hero.dirty")
              color: root.st && root.st.partial ? root.urgent : root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }
            Button {
              visible: !!root.st && !!root.st.activeProfile
              text: root.tr("hero.reapply")
              bordered: true
              foreground: root.foreground; accent: root.accent; fontFamily: root.fontFamily; fontSize: Style.font.caption
              onClicked: root.reapply()
            }
            Button {
              visible: !!root.st && root.st.dirty === true && !!root.st.activeProfile
              text: root.tr("hero.saveChanges")
              bordered: true
              foreground: root.foreground; accent: root.accent; fontFamily: root.fontFamily; fontSize: Style.font.caption
              onClicked: root.saveActive()
            }
          }

          TabStrip {
            id: tabStrip
            panel: root
            tabs: root.tabs.map(function (t) { return { key: t.key, glyph: Model.glyph(t.key === "voice" ? "voice" : t.key), label: root.tr("tab." + t.key) } })
            current: root.currentTab
            onPicked: function (key) { root.selectTab(key) }
          }

          Loader {
            id: tabBody
            width: parent.width
          }

          Text {
            width: parent.width
            wrapMode: Text.WordWrap
            text: root.tr("footer.keys")
            color: root.foreground
            opacity: 0.4
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }
        }
      }
    }
  }
}
```

- [ ] **Step 2: Syntax check and commit**

Run: `/usr/lib/qt6/bin/qmllint shell/Panel.qml 2>&1 | grep '\[syntax\]'`
Expected: no output.

```bash
git add shell/Panel.qml
git commit -m "feat(panel): hero with mute, meter, Live and profiles; tab host and keyboard cursor"
```

---

### Task 8: `VoiceTab.qml`

**Files:**
- Create: `shell/VoiceTab.qml`

**Contents:**
- the gain slider (from the descriptor's `mic.gain`)
- a 4-way noise-reduction choice
- a "Auriculares del mic" fold with the monitor enum and the headphone volume, both experimental
- with `experimental` on, a "DSP del mic (no verificado)" fold with every non-EQ `dsp.*` field, plus a nested fold with the internal EQ slots, all rendered by `FieldRows`

- [ ] **Step 1: Write `shell/VoiceTab.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui
import "Model.js" as Model

Column {
  id: tab
  property var panel: null
  readonly property var svc: panel ? panel.service : null
  readonly property var schema: panel ? panel.schema : null
  readonly property bool live: !!panel && panel.connected
  readonly property var rows: {
    var r = [gain, nr, headphones]
    if (headphones.open) r = r.concat([monitor, hpVolume])
    if (dsp.visible) {
      r.push(dsp)
      if (dsp.open) {
        r = r.concat(dspRows.rowItems())
        r.push(dspEq)
        if (dspEq.open) r = r.concat(dspEqRows.rowItems())
      }
    }
    return r
  }
  spacing: Style.spacing.xs

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("voice.title").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  MicSliderRow { id: gain; panel: tab.panel; key: "mic.gain" }
  ChipRow {
    id: nr
    panel: tab.panel
    label: tab.panel.tr("voice.nr")
    options: Model.nrOptions(tab.schema, tab.panel.tr)
    value: tab.svc ? Model.nrChoice(Model.effectiveMap(tab.svc.pending, tab.svc.mic)) : null
    interactive: tab.live
    onPicked: function (v) { tab.svc.setMic(Model.nrChanges(v)) }
  }
  Fold {
    id: headphones
    panel: tab.panel
    title: tab.panel.tr("voice.headphones")
    badge: tab.panel.tr("badge.experimental")
    MicChipRow { id: monitor; panel: tab.panel; key: "headphones.monitor" }
    MicSliderRow { id: hpVolume; panel: tab.panel; key: "headphones.volume" }
  }
  Fold {
    id: dsp
    visible: tab.panel.experimental
    panel: tab.panel
    title: tab.panel.tr("voice.dsp")
    badge: tab.panel.tr("badge.unverified")
    FieldRows { id: dspRows; panel: tab.panel; keys: Model.groupKeys(tab.schema, "dsp", "", "dsp.eq.") }
    Fold {
      id: dspEq
      panel: tab.panel
      title: tab.panel.tr("voice.dspEq")
      badge: tab.panel.tr("badge.unverified")
      FieldRows { id: dspEqRows; panel: tab.panel; keys: Model.groupKeys(tab.schema, "dsp", "dsp.eq.", "") }
    }
  }
}
```

- [ ] **Step 2: Syntax check and commit**

Run: `/usr/lib/qt6/bin/qmllint shell/VoiceTab.qml 2>&1 | grep '\[syntax\]'` (no output)

```bash
git add shell/VoiceTab.qml
git commit -m "feat(panel): Voz tab (gain, NR, mic headphones, experimental DSP)"
```

---

### Task 9: `EqTab.qml` and `BandEditor.qml`

**Files:**
- Create: `shell/EqTab.qml`
- Create: `shell/BandEditor.qml`

**Contents:**
- the live response curve
- the EQ on/off switch
- preset chips (Maono's plus the user's)
- the HPF and LPF switches and log-scale frequency sliders
- a "Bandas" fold with, per band: type chips, a log frequency slider, gain and Q
- "Guardar como preset" (a name field)
- "Borrar preset" when the active preset is a user preset

Every range comes from the filter schema.

- [ ] **Step 1: `shell/BandEditor.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui
import "Model.js" as Model

Column {
  id: band
  required property int index
  property var panel: null
  readonly property string base: "eq.bands." + index
  readonly property var rowItems: [typeRow, freqRow, gainRow, qRow]
  readonly property var typeSpec: panel ? Model.filterField(panel.schema, base + ".type") : null
  width: parent ? parent.width : 0
  spacing: Style.spacing.xxs

  PanelSectionHeader {
    width: parent.width
    text: band.panel.tr("eq.band", { n: band.index + 1 }).toUpperCase()
    foreground: band.panel.foreground
    fontFamily: band.panel.fontFamily
  }
  ChipRow {
    id: typeRow
    panel: band.panel
    label: band.panel.tr("eq.type")
    options: (band.typeSpec && band.typeSpec.options ? band.typeSpec.options : []).map(function (o) { return { value: o, label: band.panel.tr("eq." + o) } })
    value: band.panel.service ? band.panel.service.filterValue(band.base + ".type") : null
    interactive: band.panel.status.ready
    onPicked: function (v) { var c = {}; c[band.base + ".type"] = v; band.panel.service.setFilter(c) }
  }
  FilterSliderRow { id: freqRow; panel: band.panel; path: band.base + ".freq"; label: band.panel.tr("eq.freq"); logScale: true }
  FilterSliderRow { id: gainRow; panel: band.panel; path: band.base + ".gain"; label: band.panel.tr("eq.gain") }
  FilterSliderRow { id: qRow; panel: band.panel; path: band.base + ".q"; label: band.panel.tr("eq.q") }
}
```

- [ ] **Step 2: `shell/EqTab.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui
import "Model.js" as Model

Column {
  id: tab
  property var panel: null
  readonly property var svc: panel ? panel.service : null
  readonly property var f: svc ? svc.effectiveFilter() : null
  readonly property bool has: !!f
  readonly property bool ready: !!panel && panel.status.ready && has
  readonly property var presetList: svc ? svc.status.presets : []
  readonly property string presetId: has && f.eq.preset ? f.eq.preset : ""
  readonly property bool userPreset: {
    for (var i = 0; i < presetList.length; i++) if (presetList[i].id === presetId) return presetList[i].source === "user"
    return false
  }
  readonly property var rows: {
    var r = [eqOn, presets, hpfOn, hpfFreq, lpfOn, lpfFreq, bandsFold]
    if (bandsFold.open) for (var i = 0; i < bandRep.count; i++) { var b = bandRep.itemAt(i); if (b) r = r.concat(b.rowItems) }
    r.push(saveRow)
    if (userPreset) r.push(deleteRow)
    return r
  }
  spacing: Style.spacing.xs

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("eq.title").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  EqCurve { width: parent.width; panel: tab.panel; filter: tab.f }
  FilterToggleRow { id: eqOn; panel: tab.panel; path: "eq.on"; baseLabel: tab.panel.tr("eq.title") }
  ChipRow {
    id: presets
    panel: tab.panel
    label: tab.panel.tr("eq.presets") + (tab.has && !tab.f.eq.preset ? "  ·  " + tab.panel.tr("eq.custom") : "")
    options: tab.presetList.map(function (p) { return { value: p.id, label: p.name } })
    value: tab.presetId
    interactive: tab.ready
    onPicked: function (v) { tab.svc.setFilter({ "eq.preset": v }) }
  }
  FilterToggleRow { id: hpfOn; panel: tab.panel; path: "hpf.on"; baseLabel: tab.panel.tr("eq.hpf") }
  FilterSliderRow { id: hpfFreq; panel: tab.panel; path: "hpf.freq"; label: tab.panel.tr("eq.hpfFreq"); logScale: true; interactive: tab.ready && tab.f.hpf.on }
  FilterToggleRow { id: lpfOn; panel: tab.panel; path: "lpf.on"; baseLabel: tab.panel.tr("eq.lpf") }
  FilterSliderRow { id: lpfFreq; panel: tab.panel; path: "lpf.freq"; label: tab.panel.tr("eq.lpfFreq"); logScale: true; interactive: tab.ready && tab.f.lpf.on }
  Fold {
    id: bandsFold
    panel: tab.panel
    title: tab.panel.tr("eq.bands")
    Repeater {
      id: bandRep
      model: tab.has ? tab.f.eq.bands.length : 0
      BandEditor { panel: tab.panel }
    }
  }
  NameRow {
    id: saveRow
    panel: tab.panel
    placeholder: tab.panel.tr("eq.presetName")
    buttonText: tab.panel.tr("eq.savePreset")
    onSubmitted: function (name) { tab.svc.request({ cmd: "eq.preset.save", name: name }, tab.svc.report) }
  }
  ActionRow {
    id: deleteRow
    visible: tab.userPreset
    panel: tab.panel
    text: tab.panel.tr("eq.deletePreset")
    destructive: true
    onTriggered: tab.svc.request({ cmd: "eq.preset.delete", preset: tab.presetId }, tab.svc.report)
  }
}
```

`BandEditor`'s `required property int index` is set by the Repeater, which uses an integer model.

- [ ] **Step 3: Syntax check and commit**

Run: `for f in shell/EqTab.qml shell/BandEditor.qml; do /usr/lib/qt6/bin/qmllint "$f" 2>&1 | grep '\[syntax\]'; done` (no output)

```bash
git add shell/EqTab.qml shell/BandEditor.qml
git commit -m "feat(panel): EQ tab with live response curve, presets, HPF/LPF and band editors"
```

---

### Task 10: `FilterTab.qml`

**Files:**
- Create: `shell/FilterTab.qml`

**Contents:**
- the master filter switch
- the default input (clean or raw), with a note that apps that pinned their own input keep it
- RNNoise: on, VAD, grace, and retroactive grace with its latency note
- the compressor: on, threshold, ratio, attack, release, makeup
- an install hint whenever a plugin dependency is missing. The package name comes from the schema's `plugins.*.package`.

- [ ] **Step 1: Write `shell/FilterTab.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui
import "Model.js" as Model

Column {
  id: tab
  property var panel: null
  readonly property var svc: panel ? panel.service : null
  readonly property var st: panel ? panel.st : null
  readonly property var f: svc ? svc.effectiveFilter() : null
  readonly property bool ready: !!panel && panel.status.ready && !!f
  readonly property var plugins: panel && panel.schema && panel.schema.filter ? panel.schema.filter.plugins : null
  readonly property bool hasRn: !!st && !!st.deps && st.deps.rnnoise === true
  readonly property bool hasComp: !!st && !!st.deps && st.deps.comp === true
  readonly property string sourceChoice: !st || !st.defaultSource ? "" : (st.defaultSource === st.cleanSource ? "clean" : "raw")
  readonly property var rows: [enabledRow, sourceRow, rnOn, vad, grace, retro, compOn, thr, ratio, attack, release, makeup]
  spacing: Style.spacing.xs

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("filter.title").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  FilterToggleRow { id: enabledRow; panel: tab.panel; path: "enabled"; baseLabel: tab.panel.tr("filter.enabled") }
  ChipRow {
    id: sourceRow
    panel: tab.panel
    label: tab.panel.tr("filter.source")
    options: [{ value: "clean", label: tab.panel.tr("filter.sourceClean") }, { value: "raw", label: tab.panel.tr("filter.sourceRaw") }]
    value: tab.sourceChoice
    interactive: !!tab.panel && tab.panel.status.ready
    onPicked: function (v) { tab.svc.request({ cmd: "source.default", which: v }, tab.svc.report) }
  }
  HintText { panel: tab.panel; text: tab.panel.tr("filter.sourceNote") }

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("filter.rnnoise").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  HintText { visible: !!tab.st && !tab.hasRn; panel: tab.panel; color: tab.panel.urgent; text: tab.panel.tr("filter.missing", { pkg: tab.plugins ? tab.plugins.rnnoise.package : "" }) }
  FilterToggleRow { id: rnOn; panel: tab.panel; path: "rnnoise.on"; baseLabel: tab.panel.tr("filter.rnnoise"); interactive: tab.ready && tab.hasRn }
  FilterSliderRow { id: vad; panel: tab.panel; path: "rnnoise.vad"; label: tab.panel.tr("filter.vad"); interactive: tab.ready && tab.hasRn && tab.f.rnnoise.on }
  FilterSliderRow { id: grace; panel: tab.panel; path: "rnnoise.grace"; label: tab.panel.tr("filter.grace"); interactive: tab.ready && tab.hasRn && tab.f.rnnoise.on }
  FilterSliderRow { id: retro; panel: tab.panel; path: "rnnoise.retro"; label: tab.panel.tr("filter.retro"); badge: tab.panel.tr("filter.retroNote"); interactive: tab.ready && tab.hasRn && tab.f.rnnoise.on }

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("filter.comp").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  HintText { visible: !!tab.st && !tab.hasComp; panel: tab.panel; color: tab.panel.urgent; text: tab.panel.tr("filter.missing", { pkg: tab.plugins ? tab.plugins.comp.package : "" }) }
  FilterToggleRow { id: compOn; panel: tab.panel; path: "comp.on"; baseLabel: tab.panel.tr("filter.compOn"); interactive: tab.ready && tab.hasComp }
  FilterSliderRow { id: thr; panel: tab.panel; path: "comp.threshold"; label: tab.panel.tr("comp.threshold"); interactive: tab.ready && tab.hasComp && tab.f.comp.on }
  FilterSliderRow { id: ratio; panel: tab.panel; path: "comp.ratio"; label: tab.panel.tr("comp.ratio"); format: function (v) { return Model.formatValue(v, "") + ":1" }; interactive: tab.ready && tab.hasComp && tab.f.comp.on }
  FilterSliderRow { id: attack; panel: tab.panel; path: "comp.attack"; label: tab.panel.tr("comp.attack"); interactive: tab.ready && tab.hasComp && tab.f.comp.on }
  FilterSliderRow { id: release; panel: tab.panel; path: "comp.release"; label: tab.panel.tr("comp.release"); interactive: tab.ready && tab.hasComp && tab.f.comp.on }
  FilterSliderRow { id: makeup; panel: tab.panel; path: "comp.makeup"; label: tab.panel.tr("comp.makeup"); interactive: tab.ready && tab.hasComp && tab.f.comp.on }
}
```

- [ ] **Step 2: Syntax check and commit**

Run: `/usr/lib/qt6/bin/qmllint shell/FilterTab.qml 2>&1 | grep '\[syntax\]'` (no output)

```bash
git add shell/FilterTab.qml
git commit -m "feat(panel): Filtro tab (master switch, default input, RNNoise, compressor, missing deps)"
```

---

### Task 11: `LightTab.qml`

**Files:**
- Create: `shell/LightTab.qml`

**Contents:**
- on/off, effect and brightness, all from the descriptor
- preset swatches, with their hex from the schema
- swatches for the custom colours
- an HSV editor (preview, hue, saturation, value) with Add, Update and Delete for the selected custom colour

Colour selection is disabled while the effect is "cycle". That option is found by its label `effect.cycle` in the descriptor, not by number.

- [ ] **Step 1: Write `shell/LightTab.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui
import "Model.js" as Model

Column {
  id: tab
  property var panel: null
  readonly property var svc: panel ? panel.service : null
  readonly property var schema: panel ? panel.schema : null
  readonly property bool live: !!panel && panel.connected
  readonly property var lightMeta: schema && schema.device && schema.device.light ? schema.device.light : null
  readonly property var presets: lightMeta ? lightMeta.presets : []
  readonly property int customMax: lightMeta ? lightMeta.customMax : 0
  readonly property var custom: { var c = svc ? svc.value("light.custom") : null; return Array.isArray(c) ? c : [] }
  readonly property var colorValue: svc ? svc.value("light.color") : null
  readonly property var cycleValue: {
    var spec = Model.field(schema, "light.effect")
    if (!spec || !spec.options) return null
    for (var i = 0; i < spec.options.length; i++) if (spec.options[i].label === "effect.cycle") return spec.options[i].value
    return null
  }
  readonly property bool cycling: !!svc && cycleValue !== null && svc.value("light.effect") === cycleValue
  readonly property bool canPick: live && !cycling
  property real h: 0
  property real s: 1
  property real v: 1
  property int selected: -1
  readonly property var rows: [onRow, effect, brightness, presetRow, customRow, hue, sat, val, addRow].concat(selected >= 0 ? [updateRow, deleteRow] : [])
  spacing: Style.spacing.xs

  function pickCustom(value) {
    for (var i = 0; i < tab.custom.length; i++) {
      if (JSON.stringify(tab.custom[i]) === JSON.stringify(value.hsv)) {
        tab.selected = i
        tab.h = value.hsv[0]; tab.s = value.hsv[1]; tab.v = value.hsv[2]
      }
    }
    tab.svc.setMic({ "light.color": value })
  }
  function customOp(op, extra) {
    var c = { cmd: "light.custom", op: op }
    for (var k in extra) c[k] = extra[k]
    tab.svc.request(c, function (ack) { tab.svc.report(ack); if (ack.ok && op === "delete") tab.selected = -1 })
  }

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("light.title").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  MicToggleRow { id: onRow; panel: tab.panel; key: "light.on" }
  MicChipRow { id: effect; panel: tab.panel; key: "light.effect" }
  MicSliderRow { id: brightness; panel: tab.panel; key: "light.brightness" }
  HintText { visible: tab.cycling; panel: tab.panel; text: tab.panel.tr("light.cycleNote") }
  ChipRow {
    id: presetRow
    panel: tab.panel
    label: tab.panel.tr("light.colors")
    swatches: true
    options: tab.presets.map(function (p) { return { value: { preset: p.name }, label: tab.panel.tr("color." + p.name), color: p.hex } })
    value: tab.colorValue
    interactive: tab.canPick
    onPicked: function (value) { tab.selected = -1; tab.svc.setMic({ "light.color": value }) }
  }
  ChipRow {
    id: customRow
    panel: tab.panel
    label: tab.panel.tr("light.custom", { n: tab.custom.length, max: tab.customMax })
    swatches: true
    options: tab.custom.map(function (c, i) { return { value: { hsv: c }, label: "#" + (i + 1), color: Model.hsvToHex(c) } })
    value: tab.colorValue
    interactive: tab.canPick
    onPicked: function (value) { tab.pickCustom(value) }
  }

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("light.editor").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  Rectangle {
    width: parent.width
    height: Style.space(10)
    radius: height / 2
    color: Model.hsvToHex([tab.h, tab.s, tab.v])
  }
  SliderRow { id: hue; panel: tab.panel; label: tab.panel.tr("light.hue"); minimum: 0; maximum: 359; step: 1; unit: "°"; value: tab.h; onCommitted: function (x) { tab.h = x } }
  SliderRow { id: sat; panel: tab.panel; label: tab.panel.tr("light.sat"); minimum: 0; maximum: 100; step: 1; unit: "%"; value: Math.round(tab.s * 100); onCommitted: function (x) { tab.s = x / 100 } }
  SliderRow { id: val; panel: tab.panel; label: tab.panel.tr("light.val"); minimum: 0; maximum: 100; step: 1; unit: "%"; value: Math.round(tab.v * 100); onCommitted: function (x) { tab.v = x / 100 } }
  ActionRow { id: addRow; panel: tab.panel; text: tab.panel.tr("light.add"); interactive: tab.canPick && tab.custom.length < tab.customMax; onTriggered: tab.customOp("add", { hsv: [tab.h, tab.s, tab.v] }) }
  ActionRow { id: updateRow; visible: tab.selected >= 0; panel: tab.panel; text: tab.panel.tr("light.update", { n: tab.selected + 1 }); interactive: tab.live; onTriggered: tab.customOp("update", { index: tab.selected, hsv: [tab.h, tab.s, tab.v] }) }
  ActionRow { id: deleteRow; visible: tab.selected >= 0; panel: tab.panel; text: tab.panel.tr("light.delete", { n: tab.selected + 1 }); destructive: true; interactive: tab.live; onTriggered: tab.customOp("delete", { index: tab.selected }) }
}
```

The HSV ranges here (0–359°, 0–100 %) are the colour model's own domain, not mic data, so they are not "hardcoded device values". The backend validates the stored ranges.

- [ ] **Step 2: Syntax check and commit**

Run: `/usr/lib/qt6/bin/qmllint shell/LightTab.qml 2>&1 | grep '\[syntax\]'` (no output)

```bash
git add shell/LightTab.qml
git commit -m "feat(panel): Luz tab (effect, brightness, preset and custom colours, HSV editor)"
```

---

### Task 12: `ProfilesTab.qml` and `ProfileRow.qml`

**Files:**
- Create: `shell/ProfilesTab.qml`
- Create: `shell/ProfileRow.qml`

**Contents:**
- one row per profile
  - **Shows:** the name, an "activo" mark and a dot when there are unsaved changes.
  - **Actions:** apply (also with Enter), duplicate, rename (inline name field), and delete (`x`, or the button twice within 4 s).
- "Guardar estado actual como…" with group chips (mic, filtro, luz; multi-select, at least one stays on)
- the "Reaplicar al reconectar" switch, through `config.set`
- a copy button for the keybind command of the active profile, via `wl-copy`

- [ ] **Step 1: `shell/ProfileRow.qml`**

```qml
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import qs.Commons
import qs.Ui
import "Model.js" as Model

CursorSurface {
  id: row
  property var panel: null
  property var profile: ({})
  property bool active: false
  property bool dirty: false
  property bool renaming: false
  property bool armed: false
  readonly property var svc: panel ? panel.service : null
  readonly property var nameRow: renameRow

  function activate() { if (svc) svc.applyProfile(profile.id) }
  function adjust(dir) {}
  function remove() {
    if (!svc) return
    if (armed) { armed = false; svc.request({ cmd: "profile.delete", profile: profile.id }, svc.report) }
    else { armed = true; disarm.restart() }
  }

  outline: true
  current: active
  foreground: panel.foreground
  accent: panel.accent
  width: parent ? parent.width : 0
  implicitHeight: col.implicitHeight + Style.spacing.md * 2

  Timer { id: disarm; interval: 4000; onTriggered: row.armed = false }

  Column {
    id: col
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.leftMargin: Style.spacing.rowPaddingX
    anchors.rightMargin: Style.spacing.rowPaddingX
    spacing: Style.spacing.xs
    RowLayout {
      width: parent.width
      spacing: Style.spacing.sm
      Text {
        Layout.fillWidth: true
        text: row.profile.name + (row.dirty ? "  •" : "")
        elide: Text.ElideRight
        color: row.panel.foreground
        font.family: row.panel.fontFamily
        font.pixelSize: Style.font.body
        font.bold: row.active
      }
      Text {
        visible: row.active
        text: row.panel.tr("profiles.active")
        color: row.panel.accent
        font.family: row.panel.fontFamily
        font.pixelSize: Style.font.caption
      }
      PanelActionButton { iconText: Model.glyph("apply"); tooltipText: row.panel.tr("profiles.apply"); foreground: row.panel.foreground; onClicked: row.activate() }
      PanelActionButton { iconText: Model.glyph("duplicate"); tooltipText: row.panel.tr("profiles.duplicate"); foreground: row.panel.foreground; onClicked: row.svc.request({ cmd: "profile.duplicate", profile: row.profile.id }, row.svc.report) }
      PanelActionButton { iconText: Model.glyph("rename"); tooltipText: row.panel.tr("profiles.rename"); foreground: row.panel.foreground; onClicked: row.renaming = !row.renaming }
      PanelActionButton {
        iconText: Model.glyph("remove")
        tooltipText: row.armed ? row.panel.tr("profiles.confirmDelete") : row.panel.tr("profiles.delete")
        foreground: row.armed ? row.panel.urgent : row.panel.foreground
        hoverColor: row.panel.urgent
        onClicked: row.remove()
      }
    }
    Text {
      visible: row.armed
      text: row.panel.tr("profiles.confirmDelete")
      color: row.panel.urgent
      font.family: row.panel.fontFamily
      font.pixelSize: Style.font.caption
    }
    NameRow {
      id: renameRow
      visible: row.renaming
      panel: row.panel
      text: row.profile.name
      buttonText: row.panel.tr("profiles.rename")
      onSubmitted: function (name) { row.renaming = false; row.svc.request({ cmd: "profile.rename", profile: row.profile.id, name: name }, row.svc.report) }
    }
  }
  MouseArea {
    anchors.fill: parent
    acceptedButtons: Qt.NoButton
    hoverEnabled: true
    onEntered: row.panel.hoverRow(row)
  }
}
```

- [ ] **Step 2: `shell/ProfilesTab.qml`**

```qml
import QtQuick
import QtQuick.Controls
import Quickshell
import qs.Commons
import qs.Ui

Column {
  id: tab
  property var panel: null
  readonly property var svc: panel ? panel.service : null
  readonly property var st: panel ? panel.st : null
  readonly property var profiles: svc ? svc.status.profiles : []
  readonly property string keybind: st && st.activeProfile ? "maono profile apply " + st.activeProfile : ""
  property var groups: ["mic", "filter", "light"]
  readonly property var rows: {
    var r = []
    for (var i = 0; i < rep.count; i++) {
      var it = rep.itemAt(i)
      if (!it) continue
      r.push(it)
      if (it.renaming) r.push(it.nameRow)
    }
    return r.concat([saveRow, groupsRow, reconnect, copyRow])
  }
  spacing: Style.spacing.xs

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("profiles.title").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  HintText { visible: tab.profiles.length === 0; panel: tab.panel; text: tab.panel.tr("profiles.empty") }
  Repeater {
    id: rep
    model: tab.profiles
    ProfileRow {
      panel: tab.panel
      profile: modelData
      active: !!tab.st && tab.st.activeProfile === modelData.id
      dirty: active && !!tab.st && tab.st.dirty === true
    }
  }

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("profiles.save").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  NameRow {
    id: saveRow
    panel: tab.panel
    placeholder: tab.panel.tr("profiles.name")
    buttonText: tab.panel.tr("profiles.saveButton")
    onSubmitted: function (name) { tab.svc.request({ cmd: "profile.save", name: name, groups: tab.groups }, tab.svc.report) }
  }
  ChipRow {
    id: groupsRow
    panel: tab.panel
    label: tab.panel.tr("profiles.groups")
    multi: true
    options: [{ value: "mic", label: tab.panel.tr("group.mic") }, { value: "filter", label: tab.panel.tr("group.filter") }, { value: "light", label: tab.panel.tr("group.light") }]
    value: tab.groups
    onPicked: function (v) {
      var g = tab.groups.slice()
      var i = g.indexOf(v)
      if (i >= 0) { if (g.length > 1) g.splice(i, 1) } else g.push(v)
      tab.groups = g
    }
  }
  ToggleRow {
    id: reconnect
    panel: tab.panel
    baseLabel: tab.panel.tr("profiles.reapplyOnReconnect")
    description: tab.panel.tr("profiles.reapplyOnReconnectHint")
    checked: !!tab.svc && tab.svc.status.config.applyOnReconnect !== false
    interactive: !!tab.panel && tab.panel.status.ready
    onToggledTo: function (v) { tab.svc.setConfig({ applyOnReconnect: v }) }
  }
  ActionRow {
    id: copyRow
    panel: tab.panel
    text: tab.panel.tr("profiles.copy", { cmd: tab.keybind })
    interactive: tab.keybind !== ""
    onTriggered: Quickshell.execDetached(["wl-copy", tab.keybind])
  }
}
```

- [ ] **Step 3: Syntax check and commit**

Run: `for f in shell/ProfilesTab.qml shell/ProfileRow.qml; do /usr/lib/qt6/bin/qmllint "$f" 2>&1 | grep '\[syntax\]'; done` (no output)

```bash
git add shell/ProfilesTab.qml shell/ProfileRow.qml
git commit -m "feat(panel): Perfiles tab (apply, duplicate, rename, two-step delete, save with groups)"
```

---

### Task 13: `SettingsTab.qml`

**Files:**
- Create: `shell/SettingsTab.qml`

**Contents:**
- the bar actions (middle and right click)
- the wheel step, with its range from the manifest schema
- hide when disconnected
- the battery dot
- language
- the experimental DSP switch
- device info: model, firmware, serial, clean source, default input, filter health and binary
- the warnings and last notice
- "Reiniciar maono serve"

UI preferences persist through `panel.persist(...)`, which writes this widget's own `shell.json` entry.

- [ ] **Step 1: Write `shell/SettingsTab.qml`**

```qml
import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Ui
import "Model.js" as Model

Column {
  id: tab
  property var panel: null
  readonly property var svc: panel ? panel.service : null
  readonly property var st: panel ? panel.st : null
  readonly property var mic: st && st.mic ? st.mic : ({})
  readonly property var manifest: panel ? panel.manifest : null
  readonly property var wheelSpec: Model.prefSpec(manifest, "wheelStep")
  function enumOptions(key, prefix) {
    var spec = Model.prefSpec(tab.manifest, key)
    return (spec && spec.options ? spec.options : []).map(function (o) { return { value: o, label: tab.panel.tr(prefix + o) } })
  }
  readonly property var info: [
    ["info.model", st ? st.model : null],
    ["info.firmware", mic["info.firmware"]],
    ["info.serial", mic["info.serial"]],
    ["info.source", st ? st.cleanSource : null],
    ["info.defaultSource", st ? st.defaultSource : null],
    ["info.filter", st ? tab.panel.tr(st.filterApplied ? "info.filterOk" : "info.filterDown") : null],
    ["info.binary", svc ? svc.binary : null]
  ]
  readonly property var warnings: svc ? svc.status.warnings : []
  readonly property var rows: [middle, right, wheel, hide, battery, language, experimental, restartRow]
  spacing: Style.spacing.xs

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("settings.title").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  ChipRow { id: middle; panel: tab.panel; label: tab.panel.tr("settings.middle"); options: tab.enumOptions("middleClick", "action."); value: tab.panel.pref("middleClick", "mute"); onPicked: function (v) { tab.panel.persist({ middleClick: v }) } }
  ChipRow { id: right; panel: tab.panel; label: tab.panel.tr("settings.right"); options: tab.enumOptions("rightClick", "action."); value: tab.panel.pref("rightClick", "nextProfile"); onPicked: function (v) { tab.panel.persist({ rightClick: v }) } }
  SliderRow {
    id: wheel
    panel: tab.panel
    label: tab.panel.tr("settings.wheelStep")
    minimum: tab.wheelSpec && tab.wheelSpec.min !== undefined ? tab.wheelSpec.min : 1
    maximum: tab.wheelSpec && tab.wheelSpec.max !== undefined ? tab.wheelSpec.max : 1
    step: 1
    value: Number(tab.panel.pref("wheelStep", 1)) || 1
    onCommitted: function (v) { tab.panel.persist({ wheelStep: Math.round(v) }) }
  }
  ToggleRow { id: hide; panel: tab.panel; baseLabel: tab.panel.tr("settings.hide"); checked: tab.panel.pref("hideWhenDisconnected", false) === true; onToggledTo: function (v) { tab.panel.persist({ hideWhenDisconnected: v }) } }
  ToggleRow { id: battery; panel: tab.panel; baseLabel: tab.panel.tr("settings.battery"); checked: tab.panel.pref("showBattery", true) === true; onToggledTo: function (v) { tab.panel.persist({ showBattery: v }) } }
  ChipRow { id: language; panel: tab.panel; label: tab.panel.tr("settings.language"); options: tab.enumOptions("language", "lang."); value: tab.panel.pref("language", "auto"); onPicked: function (v) { tab.panel.persist({ language: v }) } }
  ToggleRow { id: experimental; panel: tab.panel; baseLabel: tab.panel.tr("settings.experimental"); description: tab.panel.tr("settings.experimentalHint"); checked: tab.panel.experimental; onToggledTo: function (v) { tab.panel.persist({ experimental: v }) } }

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("settings.device").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  Repeater {
    model: tab.info
    Text {
      width: tab.width
      leftPadding: Style.spacing.rowPaddingX
      elide: Text.ElideRight
      text: tab.panel.tr(modelData[0]) + ": " + (modelData[1] === null || modelData[1] === undefined || modelData[1] === "" ? "—" : modelData[1])
      color: tab.panel.foreground
      font.family: tab.panel.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
  }

  PanelSectionHeader { width: parent.width; text: tab.panel.tr("settings.diagnostics").toUpperCase(); foreground: tab.panel.foreground; fontFamily: tab.panel.fontFamily }
  HintText { visible: tab.warnings.length === 0; panel: tab.panel; text: tab.panel.tr("settings.noWarnings") }
  Repeater {
    model: tab.warnings
    HintText { panel: tab.panel; text: modelData }
  }
  ActionRow { id: restartRow; panel: tab.panel; text: tab.panel.tr("settings.restart"); onTriggered: tab.svc.restart() }
}
```

- [ ] **Step 2: Syntax check and commit**

Run: `/usr/lib/qt6/bin/qmllint shell/SettingsTab.qml 2>&1 | grep '\[syntax\]'` (no output)

```bash
git add shell/SettingsTab.qml
git commit -m "feat(panel): Ajustes tab (bar actions, wheel, language, experimental, device info, diagnostics)"
```

---

### Task 14: Installer for the new plugin (`src/shell.rs`)

**Files:**
- Modify: `src/shell.rs`
- Modify: `README.md` (Omarchy section)

**Interfaces:**
- `pub fn install(force: bool) -> io::Result<()>` and `pub fn uninstall() -> io::Result<()>` keep their signatures.
- New: `fn plugin_files(source: &Path) -> io::Result<Vec<PathBuf>>` returns every regular file at the top level of `shell/` with extension `qml`, `js` or `json`. `tests/` is skipped.
- New: `fn install_into(source: &Path, plugins_dir: &Path, force: bool) -> io::Result<PathBuf>`:
  - copy the files into `plugins_dir/.io.github.agusmoura.maono.new-<pid>` (a dot-dir, so the shell's watcher ignores it);
  - validate with `omarchy-plugin-validate` when it is on PATH;
  - only then move any existing target to `.…old-<pid>`, rename the new dir into place, and remove the old one.
  - It refuses if a target exists and `force` is false.
- `PLUGIN_ID = "io.github.agusmoura.maono"`

- [ ] **Step 1: Write the failing tests**

Add to `src/shell.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("maono-shell-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn source() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/shell"))
    }

    #[test]
    fn every_plugin_file_is_installed_and_tests_are_not() {
        let files = plugin_files(&source()).unwrap();
        let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        for must in ["manifest.json", "Service.qml", "BarWidget.qml", "Panel.qml", "Model.js", "Strings.js", "VoiceTab.qml"] {
            assert!(names.contains(&must.to_string()), "{must} missing from {names:?}");
        }
        assert!(!names.iter().any(|n| n.ends_with(".test.js")));
    }

    #[test]
    fn install_is_atomic_and_refuses_without_force() {
        let plugins = tmp("plugins");
        let target = install_into(&source(), &plugins, false).unwrap();
        assert_eq!(target, plugins.join(PLUGIN_ID));
        assert!(target.join("Panel.qml").is_file());
        assert!(install_into(&source(), &plugins, false).is_err());
        std::fs::write(target.join("stale.qml"), "x").unwrap();
        install_into(&source(), &plugins, true).unwrap();
        assert!(!target.join("stale.qml").exists(), "force replaces the whole folder");
        let leftovers: Vec<_> = std::fs::read_dir(&plugins).unwrap().flatten().map(|e| e.file_name()).filter(|n| n.to_string_lossy().starts_with('.')).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `mise exec rust@stable -- cargo test --lib shell` — the bin-only `shell` module is not part of the lib, so run it as `mise exec rust@stable -- cargo test --bin maono shell`.
Expected: compile errors `cannot find function plugin_files` / `install_into`.

- [ ] **Step 3: Implement**

In `src/shell.rs`:
1. Set `const PLUGIN_ID: &str = "io.github.agusmoura.maono";` and delete `const FILES`.
2. Add `plugin_files`, `validate`, and `install_into`.
3. Rewrite `install` as below. `uninstall` is unchanged, apart from now using the new id.

```rust
/// Every top-level qml/js/json file in the plugin source (never tests/).
fn plugin_files(source: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(source)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "qml" || x == "js" || x == "json"))
        .collect();
    out.sort();
    if !out.iter().any(|p| p.file_name().is_some_and(|n| n == "manifest.json")) {
        return Err(missing(format!("no manifest.json in {}", source.display())));
    }
    Ok(out)
}

/// `omarchy-plugin-validate <dir>` when Omarchy is installed; skipped otherwise.
fn validate(dir: &Path) -> io::Result<()> {
    match std::process::Command::new("omarchy-plugin-validate").arg(dir).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(io::Error::other(format!(
            "plugin validation failed: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Copy into a hidden staging dir, validate, then swap it in with renames so the
/// shell's hot-reload never sees a half-copied plugin.
fn install_into(source: &Path, plugins_dir: &Path, force: bool) -> io::Result<PathBuf> {
    let target = plugins_dir.join(PLUGIN_ID);
    if target.exists() && !force {
        return Err(io::Error::new(ErrorKind::AlreadyExists, format!("{} already exists; pass --force to replace it", target.display())));
    }
    let files = plugin_files(source)?;
    let pid = std::process::id();
    let staging = plugins_dir.join(format!(".{PLUGIN_ID}.new-{pid}"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    for f in &files {
        std::fs::copy(f, staging.join(f.file_name().unwrap()))?;
    }
    if let Err(e) = validate(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let old = plugins_dir.join(format!(".{PLUGIN_ID}.old-{pid}"));
    if target.exists() {
        std::fs::rename(&target, &old)?;
    }
    std::fs::rename(&staging, &target)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(target)
}

pub fn install(force: bool) -> io::Result<()> {
    let source = source_dir()?;
    let plugins = config_dir()?.join("omarchy").join("plugins");
    std::fs::create_dir_all(&plugins)?;
    let target = install_into(&source, &plugins, force)?;
    println!("installed {PLUGIN_ID} to {}", target.display());
    println!();
    println!("Enable it (replacing the upstream `maono` widget if you had it) with:");
    println!("  omarchy plugin disable maono      # only if the old widget is enabled");
    println!("  omarchy plugin enable {PLUGIN_ID} --section right");
    println!("  omarchy restart shell");
    Ok(())
}
```

`install_dir()` is now only used by `uninstall`; keep it.

- [ ] **Step 4: Run tests**

Run: `mise exec rust@stable -- cargo test --bin maono shell && mise exec rust@stable -- cargo test --lib`
Expected: the 2 shell tests pass, and the lib suite (99) still passes.

- [ ] **Step 5: README**

Replace the "Omarchy bar widget" section of `README.md` with a short description of the new plugin:
- id `io.github.agusmoura.maono`
- the six tabs
- `maono shell install --force`, then the three enable commands printed above
- the keybind IPC: `omarchy-shell maono toggleMute | nextProfile | applyProfile <id> | live | tab <n>`

- [ ] **Step 6: Commit**

```bash
git add src/shell.rs README.md
git commit -m "feat(shell): install the new plugin atomically and validated; README"
```

---

### Task 15: Install, migrate and verify on the desktop (REQUIRES Agus's explicit go-ahead; run together with plan 1 Task 14)

**Do not start without Agus saying yes in the conversation.** This task:
- starts `maono serve` through the Service, which rewrites the filter conf on first start and restarts `filter-chain.service`;
- writes to the mic;
- edits `shell.json`.

Do plan 1 Task 14 Steps 1–2 first (snapshot, install binary, first apply), then this.

- [ ] **Step 1: Install and migrate the widget**

```bash
cd ~/dev/maono
mise exec rust@stable -- cargo install --path . --root ~/.local --force
cp ~/.config/omarchy/shell.json /tmp/claude-1000/-home-agus/ea1adf2a-09b5-41df-ac13-a30ca564669c/scratchpad/shell.json.before
python3 -c "import json;l=json.load(open('$HOME/.config/omarchy/shell.json'))['bar']['layout']['right'];print([w['id'] for w in l].index('maono'))"
maono shell install --force
omarchy plugin disable maono
omarchy plugin enable io.github.agusmoura.maono --section right --index <index printed above>
omarchy restart shell
```

Wait ~12 s. Then:
- `journalctl --user --since -1min | grep -iE "maono|io.github.agusmoura"` should show no QML errors;
- `pgrep -af "maono serve"` should show exactly one process.

- [ ] **Step 2: Visual pass, one screenshot per tab**

For each tab `n` in 1..6, run `omarchy-shell maono tab n`, wait 1 s, then take the screenshot.
- **Capture:** use `grim` cropped to the panel only. Find its geometry with `hyprctl layers -j`, or crop the right edge under the bar.
- **Look for:**
  - every label is translated (no raw `key.names`);
  - the glyphs render;
  - the meter moves while Agus speaks;
  - the experimental fold is hidden;
  - the EQ curve is drawn.
- **Fix:** QML or glyph problems found here get fixed in a follow-up commit, re-checked with qmllint, then `omarchy restart shell`. Never trust hot reload (memory: omarchy-panel-hot-reload).

- [ ] **Step 3: Functional pass with Agus**

Each of these must be reflected both on the mic and in the panel:
1. The mute key `m` and the mic's mute button.
2. Dragging the gain slider (debounced) and the bar wheel.
3. Picking the "Gaming" profile chip: the curve changes, and the dirty dot appears after a gain change.
4. "Reaplicar".
5. Live on with headphones; then with speakers as the default output it must show the force prompt (do not force).
6. Light colour swatch: the LED changes.
7. Rename and delete a test profile (`x` twice).
8. Language → en → es.

- [ ] **Step 4: Commit fixes and push**

```bash
git push fork omarchy-panel
```

Report what was fixed, with before/after screenshots, in the task report.
