//@ pragma UseQApplication
//@ pragma Env QS_NO_RELOAD_POPUP=1
//@ pragma Env QT_QUICK_CONTROLS_STYLE=Basic
//@ pragma Env QT_QUICK_FLICKABLE_WHEEL_DECELERATION=10000

// loom's login screen: the user's own ii lock screen (side panel, clock, password characters),
// from the greeter's copy of ii (greeter.rs puts this file and LockContext.qml into it). Only the
// lock is loaded, locked from the start; the password goes to the login backend (LockContext.qml).
import "modules/common"
import "modules/ii/lock"
import "modules/ii/background"
import QtQuick
import Quickshell
import qs

ShellRoot {
    // the wallpaper and its widgets: ii's lock is see-through and blurs what is behind it
    Background {}
    Lock {
        id: lock
        // ii's own lock surface, with the desktop picker in its corner
        lockSurface: Item {
            LockSurface {
                anchors.fill: parent
                context: lock.context
            }
            SessionPicker {
                anchors.fill: parent
                context: lock.context
            }
        }
    }
    Component.onCompleted: GlobalStates.screenLocked = true
}
