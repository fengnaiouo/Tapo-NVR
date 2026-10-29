; Tapo NVR keeps its data inside the install folder (data, logs, webview, cache).
; It is removed only when the user ticks "Delete application data" while uninstalling,
; and always kept during updates (UpdateMode = 1). Recordings live in a user-chosen folder and are never removed.

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    RmDir /r "$INSTDIR\data"
    RmDir /r "$INSTDIR\logs"
    RmDir /r "$INSTDIR\webview"
    RmDir /r "$INSTDIR\cache"
    RMDir "$INSTDIR"
  ${EndIf}
!macroend
