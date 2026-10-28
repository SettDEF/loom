// loom's login screen: which desktop to start (/usr/share/wayland-sessions), a pill in the lock
// screen's corner opening a menu in ii's look. The choice goes to the backend with `start`
// (LockContext.session) and is kept for the next login.
import qs
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io

Item {
    id: root
    required property var context
    property bool open: false
    property var sessions: []   // [{ file, name }]

    readonly property var current: sessions.find(s => s.file === context.session) || sessions[0] || ({ file: "", name: "" })
    function icon(file) {
        if (file.startsWith("loom")) return "auto_awesome_mosaic"
        if (file.startsWith("hypr")) return "water_drop"
        if (file.startsWith("niri")) return "view_column"
        if (file.startsWith("plasma")) return "desktop_windows"
        if (file.startsWith("gnome")) return "footprint"
        return "monitor"
    }

    // the installed sessions: file name and Name=, loom first
    Process {
        running: true
        command: ["sh", "-c", "for f in /usr/share/wayland-sessions/*.desktop; do printf '%s\\t%s\\n' \"$(basename \"$f\")\" \"$(sed -n 's/^Name=//p' \"$f\" | head -1)\"; done"]
        stdout: StdioCollector {
            onStreamFinished: {
                const list = text.trim().split("\n").filter(l => l.includes("\t")).map(l => {
                    const [file, name] = l.split("\t")
                    return { file: file, name: name || file }
                })
                list.sort((a, b) => (b.file === "loom.desktop") - (a.file === "loom.desktop") || a.name.localeCompare(b.name))
                root.sessions = list
                if (!root.context.session && list.length > 0)
                    root.context.session = list[0].file
            }
        }
    }

    // a click outside closes the menu
    MouseArea {
        anchors.fill: parent
        visible: root.open
        onClicked: root.open = false
    }

    Rectangle {
        id: pill
        anchors { left: parent.left; bottom: parent.bottom; leftMargin: 24; bottomMargin: 24 }
        implicitWidth: pillRow.implicitWidth + 32
        implicitHeight: 48
        radius: height / 2
        color: pillArea.containsMouse || root.open ? Appearance.colors.colLayer2Hover : Appearance.colors.colLayer2
        RowLayout {
            id: pillRow
            anchors.centerIn: parent
            spacing: 10
            MaterialSymbol { text: root.icon(root.current.file); iconSize: 22; color: Appearance.colors.colOnLayer2 }
            StyledText { text: root.current.name; color: Appearance.colors.colOnLayer2; font.pixelSize: Appearance.font.pixelSize.normal }
            MaterialSymbol { text: root.open ? "expand_more" : "expand_less"; iconSize: 20; color: Appearance.colors.colOnLayer2 }
        }
        MouseArea {
            id: pillArea
            anchors.fill: parent
            hoverEnabled: true
            cursorShape: Qt.PointingHandCursor
            onClicked: root.open = !root.open
        }
    }

    Rectangle {
        id: menu
        visible: opacity > 0
        opacity: root.open ? 1 : 0
        scale: root.open ? 1 : 0.92
        transformOrigin: Item.BottomLeft
        Behavior on opacity { NumberAnimation { duration: 160; easing.type: Easing.OutCubic } }
        Behavior on scale { NumberAnimation { duration: 200; easing.type: Easing.OutCubic } }
        anchors { left: pill.left; bottom: pill.top; bottomMargin: 10 }
        width: 300
        height: list.implicitHeight + 16
        radius: Appearance.rounding.large
        color: Appearance.colors.colLayer1
        border.width: 1
        border.color: Appearance.colors.colLayer0Border

        ColumnLayout {
            id: list
            anchors { left: parent.left; right: parent.right; top: parent.top; margins: 8 }
            spacing: 2
            StyledText {
                Layout.leftMargin: 12; Layout.topMargin: 6; Layout.bottomMargin: 4
                text: "Desktop"
                color: Appearance.colors.colOnLayer1
                opacity: 0.7
                font.pixelSize: Appearance.font.pixelSize.small
            }
            Repeater {
                model: root.sessions
                delegate: Rectangle {
                    required property var modelData
                    readonly property bool chosen: modelData.file === root.context.session
                    Layout.fillWidth: true
                    implicitHeight: 48
                    radius: Appearance.rounding.normal
                    color: chosen ? Appearance.colors.colSecondaryContainer : (rowArea.containsMouse ? Appearance.colors.colLayer1Hover : "transparent")
                    RowLayout {
                        anchors { fill: parent; leftMargin: 14; rightMargin: 14 }
                        spacing: 12
                        MaterialSymbol { text: root.icon(modelData.file); iconSize: 22; color: Appearance.colors.colOnLayer1 }
                        StyledText { Layout.fillWidth: true; text: modelData.name; color: Appearance.colors.colOnLayer1; font.pixelSize: Appearance.font.pixelSize.normal }
                        MaterialSymbol { visible: chosen; text: "check"; iconSize: 20; color: Appearance.colors.colOnLayer1 }
                    }
                    MouseArea {
                        id: rowArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            root.context.session = modelData.file
                            root.open = false
                        }
                    }
                }
            }
        }
    }
}
