!macro NSIS_HOOK_POSTINSTALL
  IfFileExists "$INSTDIR\browser-extension.zip" 0 skip_ext_unzip
    CreateDirectory "$INSTDIR\browser-extension"
    FileOpen $0 "$TEMP\shiguang-extract-ext.ps1" w
    FileWrite $0 "Expand-Archive -LiteralPath '$INSTDIR\browser-extension.zip' -DestinationPath '$INSTDIR\browser-extension' -Force$\r$\n"
    FileClose $0
    ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "$TEMP\shiguang-extract-ext.ps1"'
    Delete "$TEMP\shiguang-extract-ext.ps1"
    Delete "$INSTDIR\browser-extension.zip"
  skip_ext_unzip:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  RMDir /r "$INSTDIR\browser-extension"
!macroend
