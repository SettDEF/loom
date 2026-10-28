// loom's login screen: ii's LockContext with the same API, put over ii's own in the greeter's copy
// of ii (greeter.rs; the user's files are not touched). The password goes to the login backend
// (loom-auth, LOOM_AUTH_SOCKET) for the user in greeter.json instead of PAM as this (greeter) user;
// on success the screen stays locked: loom saves its last picture and the session starts under it.
import qs
import qs.modules.common
import QtQuick
import Quickshell
import Quickshell.Io

Scope {
    id: root

    enum ActionEnum { Unlock, Poweroff, Reboot }

    signal shouldReFocus()
    signal unlocked(targetAction: var)
    signal failed()

    property string currentText: ""
    property bool unlockInProgress: false
    property bool showFailure: false
    property bool fingerprintsConfigured: false
    property var targetAction: LockContext.ActionEnum.Unlock
    property bool alsoInhibitIdle: false

    readonly property string greeterDir: Quickshell.env("LOOM_GREETER_DIR") || "/var/lib/loom/greeter"
    readonly property string authSocket: Quickshell.env("LOOM_AUTH_SOCKET") || "/run/loom/auth.sock"
    readonly property string handoverDir: Quickshell.env("LOOM_HANDOVER_DIR") || "/run/loom/handover"
    property string user: ""
    // the desktop to start: a file in /usr/share/wayland-sessions (SessionPicker)
    property string session: ""
    property string message: ""

    FileView {
        path: root.greeterDir + "/greeter.json"
        onLoaded: root.user = (JSON.parse(text()).user || "")
    }

    function resetTargetAction() { root.targetAction = LockContext.ActionEnum.Unlock }
    function clearText() { root.currentText = "" }
    function resetClearTimer() { passwordClearTimer.restart() }
    function reset() {
        root.resetTargetAction()
        root.clearText()
        root.unlockInProgress = false
    }
    // no fingerprint before login (fprintd as the greeter user does not know the user's prints)
    function tryFingerUnlock() {}
    function stopFingerPam() {}

    Timer {
        id: passwordClearTimer
        interval: 10000
        onTriggered: if (!root.unlockInProgress) root.reset()
    }

    onCurrentTextChanged: {
        if (currentText.length > 0) {
            showFailure = false
            GlobalStates.screenUnlockFailed = false
        }
        GlobalStates.screenLockContainsCharacters = currentText.length > 0
        passwordClearTimer.restart()
    }

    function fail(m) {
        root.message = m || ""
        root.clearText()
        root.unlockInProgress = false
        GlobalStates.screenUnlockFailed = true
        root.showFailure = true
        root.failed()
    }

    function tryUnlock(alsoInhibitIdle = false) {
        if (root.unlockInProgress)
            return
        root.alsoInhibitIdle = alsoInhibitIdle
        // power off / reboot from the side panel need no password before login
        if (root.targetAction !== LockContext.ActionEnum.Unlock) {
            root.unlocked(root.targetAction)
            return
        }
        root.unlockInProgress = true
        auth.connected = true
    }

    Socket {
        id: auth
        path: root.authSocket
        onConnectionStateChanged: {
            if (connected) {
                write(JSON.stringify({ user: root.user, password: root.currentText }) + "\n")
                flush()
            }
        }
        onError: root.fail("The sign-in service is not running")
        parser: SplitParser {
            onRead: line => {
                let r = {}
                try { r = JSON.parse(line) } catch (e) {}
                if (r.started !== undefined) {
                    auth.connected = false
                    if (!r.started) root.fail(r.message || "The session did not start")
                    else console.log("greeter: session started for " + root.user)
                } else if (r.ok) {
                    // signed in: the last picture of each screen first (the session's curtain)
                    root.clearText()
                    shots.running = true
                } else {
                    auth.connected = false
                    root.fail(r.message)
                }
            }
        }
    }

    // one picture per screen, named as loom names it (curtain.rs reads <output>.png)
    Process {
        id: shots
        command: ["sh", "-c", "mkdir -p \"$0\" && for o in $(echo \"$1\"); do grim -o \"$o\" \"$0/$o.png\" || exit 1; done", root.handoverDir, Quickshell.screens.map(s => s.name).join(" ")]
        onExited: (code, status) => {
            if (code !== 0) console.warn("greeter: no last picture (" + code + "), the session starts without its curtain")
            auth.write(JSON.stringify({ start: true, session: root.session }) + "\n")
            auth.flush()
        }
    }
}
