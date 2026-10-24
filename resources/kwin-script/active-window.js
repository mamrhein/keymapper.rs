// ---------------------------------------------------------------------------
// keymapper KWin script: reports the resource class (WM_CLASS) of the active
// window back to a keymapper process over D-Bus.
//
// Installed by scripts/install-kwin-script.sh to
// $XDG_DATA_HOME/keymapperd/kwin/active-window.js.
//
// This file is a template, not a persistent KWin package. A KWin script
// cannot export D-Bus methods or emit signals, so a script loaded once at
// session start could never answer queries on demand. Instead, keymapperd
// replaces every `@REPLY_DEST@` placeholder below with the unique D-Bus name
// of a temporary reply sink, stages the result under its runtime directory,
// and loads it through KWin's scripting interface
// (`org.kde.KWin /Scripting`, `loadScript` + `run`). The callback lands on
// the `de.adrhinum.keymapper.ActiveWindowReply` interface served by that
// sink.
//
// Keep the placeholder when customizing this script: a copy without it is
// ignored and keymapperd falls back to the identical copy embedded in the
// binary.
// ---------------------------------------------------------------------------

const dest = "@REPLY_DEST@";
const replyInterface = "de.adrhinum.keymapper.ActiveWindowReply";
function output_result(message) {
  callDBus(dest, "/", replyInterface, "result", message.toString());
}
function output_error(message) {
  print("keymapper active-window script: " + message.toString());
  callDBus(dest, "/", replyInterface, "error", message.toString());
}
try {
  const w = workspace.activeWindow;
  if (w == null) {
    output_error("No active window");
  } else {
    output_result(JSON.stringify({ class_name: w.resourceClass }));
  }
} catch (e) {
  output_error("Script error: " + e.toString());
}
