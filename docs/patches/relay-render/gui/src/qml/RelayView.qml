import QtQuick
import org.streetpea.chiaking

// Relay owns every control and dialog. This root contributes no native widgets,
// shortcut handlers, loading hints, settings loaders or extra windows.
Item {
    objectName: "relay-video-surface"
    anchors.fill: parent

    Component.onCompleted: Chiaki.window.keepVideo = true
    Connections {
        target: Chiaki.window
        function onLoadingTransitionCompleteChanged() {
            if (Chiaki.window.loadingTransitionComplete)
                Chiaki.window.noteLoadingTransitionComplete();
        }
    }
}
