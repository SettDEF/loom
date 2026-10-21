// KWin's scripted-effect API (the part the shipped effects use), for loom. Loaded before an
// effect's main.js in its own context. Windows are plain objects keyed by loom's address; loom
// sends events with their current properties (__loom.event) and runs the animations natively.
"use strict";

function Signal() { this._f = []; }
Signal.prototype.connect = function (f) { this._f.push(f); };
Signal.prototype.disconnect = function (f) { this._f = this._f.filter(function (x) { return x !== f; }); };
Signal.prototype.emit = function () {
    var a = arguments;
    this._f.slice().forEach(function (f) { try { f.apply(null, a); } catch (e) { __loom_log("signal: " + e + "\n" + (e.stack || "")); } });
};

// AnimationEffect::Attribute, MetaType anchors, directions, data roles (KWin's values)
var Effect = {
    Opacity: 0, Brightness: 1, Saturation: 2, Scale: 3, Rotation: 4, Position: 5, Size: 6,
    Translation: 7, Clip: 8, Generic: 9, CrossFadePrevious: 10, Shader: 11, ShaderUniform: 12,
    Forward: 0, Backward: 1,
    TerminateAtSource: 0, TerminateAtTarget: 1, DontTerminate: 2,
    WindowAddedGrabRole: 1, WindowClosedGrabRole: 2, WindowMinimizedGrabRole: 3, WindowUnminimizedGrabRole: 4,
    WindowForceBlurRole: 5, WindowBlurBehindRole: 6, WindowForceBackgroundContrastRole: 7, WindowBackgroundContrastRole: 8,
    Left: 1, Top: 2, Right: 4, Bottom: 8, Horizontal: 5, Vertical: 10, Mouse: 16,
};

var QEasingCurve = {
    Linear: 0, InQuad: 1, OutQuad: 2, InOutQuad: 3, OutInQuad: 4, InCubic: 5, OutCubic: 6, InOutCubic: 7,
    OutInCubic: 8, InQuart: 9, OutQuart: 10, InOutQuart: 11, OutInQuart: 12, InQuint: 13, OutQuint: 14,
    InOutQuint: 15, OutInQuint: 16, InSine: 17, OutSine: 18, InOutSine: 19, OutInSine: 20, InExpo: 21,
    OutExpo: 22, InOutExpo: 23, OutInExpo: 24, InCirc: 25, OutCirc: 26, InOutCirc: 27, OutInCirc: 28,
    InElastic: 29, OutElastic: 30, InOutElastic: 31, OutInElastic: 32, InBack: 33, OutBack: 34,
    InOutBack: 35, OutInBack: 36, InBounce: 37, OutBounce: 38, InOutBounce: 39, OutInBounce: 40,
};

// any `…Changed` (or other) signal a script connects to exists on first use, as KWin has one per property
function __signals(obj) {
    return new Proxy(obj, { get: function (t, p) { if (!(p in t) && typeof p === "string" && /(Changed|Added|Removed|Closed|Activated|Started|Finished)$/.test(p)) t[p] = new Signal(); return t[p]; } });
}

var effects = {
    windowAdded: new Signal(), windowClosed: new Signal(), windowMinimized: new Signal(),
    windowUnminimized: new Signal(), windowMaximizedStateChanged: new Signal(),
    windowFrameGeometryChanged: new Signal(), windowDataChanged: new Signal(), windowActivated: new Signal(),
    windowStartUserMovedResized: new Signal(), windowFinishUserMovedResized: new Signal(),
    windowFullScreenChanged: new Signal(), desktopChanged: new Signal(), showingDesktopChanged: new Signal(),
    activeFullScreenEffectChanged: new Signal(), screenLockingChanged: new Signal(),
    hasActiveFullScreenEffect: false, showingDesktop: false, stackingOrder: [], activeWindow: null,
    animationTimeFactor: 1,
    colorPickerActive: false,
};
effects = __signals(effects);

var __config = {};
var effect = {
    configChanged: new Signal(), animationEnded: new Signal(),
    readConfig: function (k, d) { return __config[k] !== undefined ? __config[k] : d; },
    isGrabbed: function () { return false; }, grab: function () { return true; }, ungrab: function () { return true; },
    // screen edges, shortcuts, shaders: accepted (loom has none of them for scripts yet)
    touchEdgesForAction: function () { return []; }, registerShortcut: function () {}, registerScreenEdge: function () { return false; },
    registerTouchScreenEdge: function () { return false; }, unregisterScreenEdge: function () { return false; }, unregisterTouchScreenEdge: function () { return false; },
    addFragmentShader: function () { return 0; }, setUniform: function () {},
};
effect = __signals(effect);

function animationTime(ms) {
    if (typeof ms === "object" && ms !== null) ms = ms.duration || 200;
    return Math.max(1, ms * effects.animationTimeFactor);
}

var __windows = {};
function __win(d) {
    var w = __windows[d.addr];
    if (!w) {
        w = __signals({ addr: d.addr, __data: {} });
        w.setData = function (role, v) { this.__data[role] = v; };
        w.data = function (role) { return this.__data[role]; };
        __windows[d.addr] = w;
    }
    for (var k in d) w[k] = d[k];
    return w;
}

function __args(o, keep) {
    var anims = o.animations ? o.animations : [o];
    var out = {
        window: o.window ? o.window.addr : 0, keep: !!keep, keepAlive: o.keepAlive !== false,
        duration: o.duration || 200, delay: o.delay || 0, curve: o.curve !== undefined ? o.curve : 0,
        animations: anims.map(function (a) {
            return { type: a.type, from: a.from, to: a.to, curve: a.curve, duration: a.duration, delay: a.delay };
        }),
    };
    return JSON.stringify(out);
}
function animate(o) { return __loom_animate(__args(o, false)); }
function set(o) { return __loom_animate(__args(o, true)); }
function __ids(id) { return Array.isArray(id) ? id : [id]; }
function cancel(id) { if (id === undefined || id === null) return false; var r = false; __ids(id).forEach(function (i) { r = __loom_cancel(i) || r; }); return r; }
function complete(id) { if (id === undefined || id === null) return false; var r = false; __ids(id).forEach(function (i) { r = __loom_complete(i) || r; }); return r; }
function redirect(id, dir, term) { if (id === undefined || id === null) return false; var r = false; __ids(id).forEach(function (i) { r = __loom_redirect(i, dir || 0, term === undefined ? 1 : term) || r; }); return r; }
function retarget(id, to, remaining) { if (id === undefined || id === null) return false; var r = false; __ids(id).forEach(function (i) { r = __loom_retarget(i, JSON.stringify(to), remaining === undefined ? -1 : remaining) || r; }); return r; }
function displayWidth() { return __loom_display[0]; }
function displayHeight() { return __loom_display[1]; }
var print = function () { __loom_log(Array.prototype.join.call(arguments, " ")); };
var console = { log: print, warn: print, error: print };

// from loom: an event with the window's properties now (and its stacking order, active window)
var __loom = {
    event: function (name, json, extra) {
        var d = JSON.parse(json);
        effects.stackingOrder = d.stacking.map(function (s) { return __win(s); });
        effects.activeWindow = d.active ? __windows[d.active] || null : null;
        var w = d.window ? __win(d.window) : null;
        var x = extra ? JSON.parse(extra) : [];
        if (effects[name]) effects[name].emit.apply(effects[name], [w].concat(x));
        if (name === "windowClosed" && w) w.deleted = true;
    },
    ended: function (addr, id) {
        var w = __windows[addr];
        if (w) effect.animationEnded.emit(w, id);
    },
    forget: function (addr) { delete __windows[addr]; },
};
