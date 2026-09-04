# 向 tokenspeed 悬浮条窗口发送托盘回调消息（WM_APP+1, lParam=WM_RBUTTONUP）以弹出托盘菜单
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll")] public static extern IntPtr PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
'@
$p = Get-Process tokenspeed -ErrorAction Stop
$h = [IntPtr]$p.MainWindowHandle
if ($h -eq [IntPtr]::Zero) { Write-Output "no main window"; exit 1 }
$ok = [W.U]::PostMessageW($h, 0x8001, [IntPtr]1, [IntPtr]0x0205)
Write-Output "posted=$ok hwnd=$h pid=$($p.Id)"
