# Copyright 2018-2026 the Deno authors. MIT license.
# Click one of an app's toasts the way Windows does: CoCreateInstance on the
# COM activator the app registered for its AppUserModelID (a running app
# serves it; otherwise COM starts the app with -ToastActivated -Embedding),
# then INotificationActivationCallback::Activate with the toast's arguments.
# The same round trip as laufey's scripts/notification-coldstart-e2e.ps1.
#
#   powershell -File toast-click.ps1 -Aumid <app id> -Tag <tag> [-Action <id>] [-Data <json>]
param(
  [Parameter(Mandatory = $true)][string]$Aumid,
  [Parameter(Mandatory = $true)][string]$Tag,
  [string]$Action = "",
  [string]$Data = ""
)
$ErrorActionPreference = "Stop"
$clsid = (Get-ItemProperty "HKCU:\Software\Classes\AppUserModelId\$Aumid").CustomActivator
if (-not $clsid) { throw "no toast activator registered for $Aumid" }
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("53E31837-6600-4A81-9395-75CFFE746F94"),
 InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface INotificationActivationCallback {
  void Activate([MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
                [MarshalAs(UnmanagedType.LPWStr)] string invokedArgs,
                IntPtr data, uint count);
}
public static class ToastClick {
  public static void Click(string clsid, string aumid, string args) {
    object server = Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid(clsid)));
    try {
      ((INotificationActivationCallback)server).Activate(aumid, args, IntPtr.Zero, 0);
    } finally {
      Marshal.ReleaseComObject(server);
    }
  }
}
"@
$invoked = "laufey=1&tag=" + [Uri]::EscapeDataString($Tag)
if ($Action) { $invoked += "&action=" + [Uri]::EscapeDataString($Action) }
if ($Data) { $invoked += "&data=" + [Uri]::EscapeDataString($Data) }
[ToastClick]::Click($clsid, $Aumid, $invoked)
Write-Host "clicked $Tag ($Action) via $clsid"
