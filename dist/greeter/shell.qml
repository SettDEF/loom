// loom's login screen (docs/greeter.md): the lock screen's pieces in the user's look, from the
// snapshot loom keeps (LOOM_GREETER_DIR/greeter.json: colours, font, clock, wallpaper, name).
import QtQuick
import QtQuick.Effects
import Quickshell
import Quickshell.Io
import Quickshell.Wayland

ShellRoot {
    id: root
    readonly property string dir: Quickshell.env("LOOM_GREETER_DIR") || "/var/lib/loom/greeter"
    property var g: ({})
    readonly property var c: g.colors || ({})
    function col(name, fallback) { return c[name] || fallback }
    // the password, once per screen's field; one shared text so every screen shows the same dots
    property string password: ""
    property bool busy: false
    property string message: ""

    FileView {
        path: root.dir + "/greeter.json"
        onLoaded: root.g = JSON.parse(text())
    }

    // `submit`: the password to loom-auth over its socket (LOOM_AUTH_SOCKET), straight from this
    // page: it never passes through the compositor
    readonly property string authSocket: Quickshell.env("LOOM_AUTH_SOCKET") || "/run/loom/auth.sock"
    signal failed()
    function submit() {
        if (password.length === 0 || busy)
            return
        busy = true
        message = "Signing in…"
        auth.connected = true
    }
    Socket {
        id: auth
        path: root.authSocket
        onConnectionStateChanged: {
            if (connected) {
                write(JSON.stringify({ user: root.g.user || "", password: root.password }) + "\n")
                flush()
            }
        }
        onError: {
            root.busy = false
            root.message = "The sign-in service is not running"
        }
        parser: SplitParser {
            onRead: line => {
                auth.connected = false
                let r = {}
                try { r = JSON.parse(line) } catch (e) {}
                root.password = ""
                if (r.ok) {
                    root.message = "Welcome"
                    console.log("greeter: signed in as " + (root.g.user || "?"))
                } else {
                    root.busy = false
                    root.message = r.message || "Sign-in failed"
                    root.failed()
                }
            }
        }
    }

    SystemClock { id: clock; precision: SystemClock.Seconds }

    Variants {
        model: Quickshell.screens
        PanelWindow {
            required property var modelData
            screen: modelData
            anchors { top: true; bottom: true; left: true; right: true }
            exclusionMode: ExclusionMode.Ignore
            WlrLayershell.layer: WlrLayer.Overlay
            WlrLayershell.namespace: "loom:greeter"
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
            color: root.col("background", "#111318")

            Image {
                id: wall
                anchors.fill: parent
                source: root.g.wallpaper ? "file://" + root.dir + "/" + root.g.wallpaper : ""
                fillMode: Image.PreserveAspectCrop
                visible: false
            }
            MultiEffect {
                anchors.fill: parent
                source: wall
                visible: wall.status === Image.Ready
                blurEnabled: true
                blurMax: 64
                blur: 0.6
                brightness: -0.15
            }

            Column {
                anchors.horizontalCenter: parent.horizontalCenter
                y: parent.height * 0.18
                spacing: 4
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: Qt.formatTime(clock.date, "hh:mm")
                    color: root.col("on_background", "white")
                    font.family: (root.g.clock && root.g.clock.family) || root.g.font || "sans-serif"
                    font.pixelSize: ((root.g.clock && root.g.clock.size) || 90) * 1.4
                    font.weight: (root.g.clock && root.g.clock.weight) || 350
                }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: Qt.formatDate(clock.date, "dddd, d MMMM")
                    color: root.col("on_background", "white")
                    font.family: root.g.font || "sans-serif"
                    font.pixelSize: 22
                }
            }

            Column {
                anchors.horizontalCenter: parent.horizontalCenter
                anchors.bottom: parent.bottom
                anchors.bottomMargin: parent.height * 0.18
                spacing: 14

                Rectangle {
                    anchors.horizontalCenter: parent.horizontalCenter
                    width: 72; height: 72; radius: 36
                    color: root.col("primary_container", "#334")
                    Text {
                        anchors.centerIn: parent
                        text: (root.g.name || root.g.user || "?").charAt(0).toUpperCase()
                        color: root.col("on_primary_container", "white")
                        font.family: root.g.font || "sans-serif"
                        font.pixelSize: 32
                    }
                }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: root.g.name || root.g.user || ""
                    color: root.col("on_background", "white")
                    font.family: root.g.font || "sans-serif"
                    font.pixelSize: 20
                }
                Rectangle {
                    id: pill
                    anchors.horizontalCenter: parent.horizontalCenter
                    width: 300; height: 48; radius: 24
                    transform: Translate { id: shake }
                    SequentialAnimation {
                        id: shakeAnim
                        NumberAnimation { target: shake; property: "x"; to: -12; duration: 50 }
                        NumberAnimation { target: shake; property: "x"; to: 12; duration: 80 }
                        NumberAnimation { target: shake; property: "x"; to: -6; duration: 70 }
                        NumberAnimation { target: shake; property: "x"; to: 0; duration: 60 }
                    }
                    Connections {
                        target: root
                        function onFailed() { shakeAnim.start(); field.forceActiveFocus() }
                    }
                    color: root.col("surface_container_high", "#2a2d33")
                    TextInput {
                        id: field
                        anchors.fill: parent
                        anchors.leftMargin: 20; anchors.rightMargin: 20
                        verticalAlignment: TextInput.AlignVCenter
                        echoMode: TextInput.Password
                        passwordCharacter: "●"
                        color: root.col("on_surface", "white")
                        font.family: root.g.font || "sans-serif"
                        font.pixelSize: 18
                        focus: true
                        enabled: !root.busy
                        text: root.password
                        onTextEdited: root.password = text
                        onAccepted: root.submit()
                    }
                    Text {
                        anchors.centerIn: parent
                        visible: root.password.length === 0
                        text: "Password"
                        color: root.col("on_surface_variant", "#aaa")
                        font.family: root.g.font || "sans-serif"
                        font.pixelSize: 16
                    }
                }
                Text {
                    anchors.horizontalCenter: parent.horizontalCenter
                    text: root.message
                    color: root.col("on_background", "white")
                    font.family: root.g.font || "sans-serif"
                    font.pixelSize: 14
                }
            }
        }
    }
}
