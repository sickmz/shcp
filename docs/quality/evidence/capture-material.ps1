# Sizes the shcp window and saves a picture of it, without touching anything inside it.
#   .\capture.ps1 -Out docs\both.png [-Width 1272] [-Height 795]
param(
    [Parameter(Mandatory)][string]$Out,
    [int]$Width = 1272,
    [int]$Height = 795
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ShcpWindow {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr window, int attribute, out Rect rect, int size);
}
'@
[void][ShcpWindow]::SetProcessDPIAware()
$process = Get-Process terminal_material | Where-Object MainWindowHandle -ne 0 | Select-Object -First 1
if (-not $process) {
    throw 'No shcp window.'
}
$window = $process.MainWindowHandle
[void][ShcpWindow]::ShowWindow($window, 9)
[void][ShcpWindow]::SetWindowPos($window, [IntPtr]::Zero, 80, 80, $Width, $Height, 0x0014)
Start-Sleep -Seconds 2
$rect = New-Object ShcpWindow+Rect
[void][ShcpWindow]::GetWindowRect($window, [ref]$rect)
$bitmap = New-Object Drawing.Bitmap ($rect.Right - $rect.Left), ($rect.Bottom - $rect.Top)
$graphics = [Drawing.Graphics]::FromImage($bitmap)
$dc = $graphics.GetHdc()
# 2: what the graphics card draws, which a plain copy of the window would miss.
$done = [ShcpWindow]::PrintWindow($window, $dc, 2)
$graphics.ReleaseHdc($dc)
$graphics.Dispose()
if (-not $done) {
    throw 'Could not capture the window.'
}
# Without the invisible border Windows keeps around a window to grab it by.
$frame = New-Object ShcpWindow+Rect
[void][ShcpWindow]::DwmGetWindowAttribute($window, 9, [ref]$frame, 16)
$visible = New-Object Drawing.Rectangle ($frame.Left - $rect.Left), ($frame.Top - $rect.Top), ($frame.Right - $frame.Left), ($frame.Bottom - $frame.Top)
$picture = $bitmap.Clone($visible, $bitmap.PixelFormat)
$bitmap.Dispose()
$path = Join-Path (Get-Location) $Out
$picture.Save($path, [Drawing.Imaging.ImageFormat]::Png)
$picture.Dispose()
Write-Output "Saved $path"

