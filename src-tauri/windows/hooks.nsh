; NSIS installer hooks — wired in via bundle.windows.nsis.installerHooks.
;
; The binary was renamed loomux.exe -> orrerix.exe (#1562). tauri-bundler's
; installer.nsi already handles the normal upgrade by itself: it records
; MainBinaryName under the (productName-keyed, so unchanged) uninstall key, and
; the next install reads it back and deletes the old exe when the name differs.
;
; What it does NOT handle is the old exe still RUNNING. Its own guard,
;
;   !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
;
; asks about the NEW name only, and the delete it performs has no /REBOOTOK — so
; installing by hand (setup.exe, install.ps1) while the previous build is open
; leaves that delete silently failing and both executables in $INSTDIR. The
; launcher's own refuseIfRunning probes both names and closes this off the
; `orrerix update` path; this closes it off the hand-install path too.
;
; Same macro, same message, applied to the previous name. NSIS_HOOK_PREINSTALL
; runs at the top of Section Install, before the bundler's own check and before
; any file is copied, so a refusal here costs nothing but the user's "quit it
; and try again" (verified against installer.nsi at tag tauri-cli-v2.11.4, the
; version package-lock.json pins).
;
; This is this product's own previous binary, not a toolchain assumption — it
; can be dropped once no supported upgrade path starts from a pre-#1562 build.

!macro NSIS_HOOK_PREINSTALL
  !insertmacro CheckIfAppIsRunning "loomux.exe" "${PRODUCTNAME}"
!macroend
