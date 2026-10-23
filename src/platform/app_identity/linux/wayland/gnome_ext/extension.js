import Extension from "resource:///org/gnome/shell/extensions/extension.js";
import Gio from "gi://Gio";
import Shell from "gi://Shell";

// Define the D-Bus introspection XML.
const DBUS_INTERFACE_XML = `
<node>
  <interface name="org.gnome.shell.extensions.ActiveWindow">
    <method name="GetActiveWindow">
      <arg type="s" direction="out" name="window_info"/>
    </method>
  </interface>
</node>`;

export default class ActiveWindowExtension extends Extension {
  // Triggered automatically when the user enables the extension
  enable() {
    // Wrap this class instance into an exportable D-Bus object using the XML schema
    this._dbusImpl = Gio.DBusExportedObject.wrapJSObject(
      DBUS_INTERFACE_XML,
      this,
    );

    // Export the object to the user's Session Bus so Rust can find it
    this._dbusImpl.export(
      Gio.DBus.session,
      "/org/gnome/shell/extensions/ActiveWindow",
    );
  }

  // Triggered automatically when the user disables the extension.
  disable() {
    if (this._dbusImpl) {
      // Unregister the D-Bus interface path
      this._dbusImpl.unexport();
      this._dbusImpl = null;
    }
  }

  /**
   * The core D-Bus method exposed to the system.
   *
   * @returns {string} A JSON string containing window details or an error message.
   */
  GetActiveWindow() {
    // Retrieve the currently focused window from the global Mutter display manager
    let activeWindow = global.display.focus_window;

    // If no window has keyboard focus (e.g., clicking on the empty desktop background)
    if (!activeWindow) {
      return JSON.stringify({ error: "No active window found" });
    }

    // Use the GNOME Shell WindowTracker utility to map the window to its application
    let tracker = Shell.WindowTracker.get_default();
    let app = tracker.get_window_app(activeWindow);

    // Return window and application telemetry structured as a JSON string
    return JSON.stringify({
      title: activeWindow.get_title(), // The title text of the window
      wm_class: activeWindow.get_wm_class(), // The window manager class name
      app_id: app ? app.get_id() : "unknown", // The .desktop file ID (e.g., "firefox.desktop")
    });
  }
}
