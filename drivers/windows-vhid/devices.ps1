# devices.ps1: funciones comunes de install.ps1 y uninstall.ps1 (se cargan con ". .\devices.ps1").
#
# Crean y quitan el dispositivo raíz Root\coroomvhid con SetupAPI, como hace "devcon install" /
# "devcon remove", para no depender de devcon.exe ni devgen.exe (vienen con el WDK, no con
# Windows).

Set-StrictMode -Version Latest

$CoroomHardwareId = 'Root\coroomvhid'
# Clase System (la del INF).
$CoroomClassGuid = [Guid]'4d36e97d-e325-11ce-bfc1-08002be10318'

if (-not ('CoroomVhid.SetupApi' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace CoroomVhid {
    public static class SetupApi {
        [StructLayout(LayoutKind.Sequential)]
        struct SP_DEVINFO_DATA {
            public uint cbSize;
            public Guid ClassGuid;
            public uint DevInst;
            public IntPtr Reserved;
        }

        const uint DICD_GENERATE_ID = 0x00000001;
        const uint SPDRP_HARDWAREID = 0x00000001;
        const uint DIF_REMOVE = 0x00000005;
        const uint DIF_REGISTERDEVICE = 0x00000019;
        static readonly IntPtr INVALID_HANDLE_VALUE = new IntPtr(-1);

        [DllImport("setupapi.dll", SetLastError = true)]
        static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid ClassGuid, IntPtr hwndParent);

        [DllImport("setupapi.dll", SetLastError = true)]
        static extern IntPtr SetupDiCreateDeviceInfoList(IntPtr ClassGuid, IntPtr hwndParent);

        [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern bool SetupDiCreateDeviceInfoW(IntPtr DeviceInfoSet, string DeviceName,
            ref Guid ClassGuid, string DeviceDescription, IntPtr hwndParent, uint CreationFlags,
            ref SP_DEVINFO_DATA DeviceInfoData);

        [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern bool SetupDiOpenDeviceInfoW(IntPtr DeviceInfoSet, string DeviceInstanceId,
            IntPtr hwndParent, uint OpenFlags, ref SP_DEVINFO_DATA DeviceInfoData);

        [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr DeviceInfoSet,
            ref SP_DEVINFO_DATA DeviceInfoData, uint Property, byte[] PropertyBuffer,
            uint PropertyBufferSize);

        [DllImport("setupapi.dll", SetLastError = true)]
        static extern bool SetupDiCallClassInstaller(uint InstallFunction, IntPtr DeviceInfoSet,
            ref SP_DEVINFO_DATA DeviceInfoData);

        [DllImport("setupapi.dll", SetLastError = true)]
        static extern bool SetupDiDestroyDeviceInfoList(IntPtr DeviceInfoSet);

        static Win32Exception LastError(string what) {
            int code = Marshal.GetLastWin32Error();
            return new Win32Exception(code, what + ": " + new Win32Exception(code).Message);
        }

        // Crea un dispositivo raíz ROOT\<CLASE>\NNNN con ese identificador de hardware (sin
        // driver: "pnputil /add-driver ... /install" se lo instala después).
        public static void CreateRootDevice(Guid classGuid, string className, string hardwareId,
                                            string description) {
            IntPtr set = SetupDiCreateDeviceInfoList(ref classGuid, IntPtr.Zero);
            if (set == INVALID_HANDLE_VALUE) {
                throw LastError("SetupDiCreateDeviceInfoList");
            }
            try {
                SP_DEVINFO_DATA data = new SP_DEVINFO_DATA();
                data.cbSize = (uint)Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
                if (!SetupDiCreateDeviceInfoW(set, className, ref classGuid, description,
                                              IntPtr.Zero, DICD_GENERATE_ID, ref data)) {
                    throw LastError("SetupDiCreateDeviceInfo");
                }
                // REG_MULTI_SZ: el identificador y dos NUL.
                byte[] ids = Encoding.Unicode.GetBytes(hardwareId + "\0\0");
                if (!SetupDiSetDeviceRegistryPropertyW(set, ref data, SPDRP_HARDWAREID, ids,
                                                       (uint)ids.Length)) {
                    throw LastError("SetupDiSetDeviceRegistryProperty");
                }
                if (!SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, ref data)) {
                    throw LastError("SetupDiCallClassInstaller(DIF_REGISTERDEVICE)");
                }
            } finally {
                SetupDiDestroyDeviceInfoList(set);
            }
        }

        // Quita un dispositivo por su identificador de instancia (ROOT\SYSTEM\0001).
        public static void RemoveDevice(string instanceId) {
            IntPtr set = SetupDiCreateDeviceInfoList(IntPtr.Zero, IntPtr.Zero);
            if (set == INVALID_HANDLE_VALUE) {
                throw LastError("SetupDiCreateDeviceInfoList");
            }
            try {
                SP_DEVINFO_DATA data = new SP_DEVINFO_DATA();
                data.cbSize = (uint)Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
                if (!SetupDiOpenDeviceInfoW(set, instanceId, IntPtr.Zero, 0, ref data)) {
                    throw LastError("SetupDiOpenDeviceInfo(" + instanceId + ")");
                }
                if (!SetupDiCallClassInstaller(DIF_REMOVE, set, ref data)) {
                    throw LastError("SetupDiCallClassInstaller(DIF_REMOVE)");
                }
            } finally {
                SetupDiDestroyDeviceInfoList(set);
            }
        }
    }
}
'@
}

function Assert-Administrator {
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Hace falta una consola de PowerShell abierta como administrador.'
    }
}

# Dispositivos con el identificador de hardware del driver (presentes o no).
function Get-CoroomDevice {
    Get-PnpDevice -ErrorAction SilentlyContinue |
        Where-Object { $_.HardwareID -contains $CoroomHardwareId }
}

# Paquetes del driver en el almacén de drivers (oemNN.inf).
function Get-CoroomDriverPackage {
    Get-WindowsDriver -Online -ErrorAction SilentlyContinue |
        Where-Object { (Split-Path -Leaf $_.OriginalFileName) -ieq 'coroomvhid.inf' }
}

# Indica si Windows arrancó con la firma de prueba activada (bcdedit /set testsigning on).
function Test-TestSigning {
    $options = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control' -Name SystemStartOptions -ErrorAction SilentlyContinue).SystemStartOptions
    return [bool]($options -match 'TESTSIGNING')
}

# Ejecuta pnputil y devuelve su código de salida. 0: bien; 3010 (ERROR_SUCCESS_REBOOT_REQUIRED):
# bien, con reinicio pendiente; los de -AlsoAccept se aceptan también. Cualquier otro es un error.
function Invoke-PnpUtil {
    param(
        [Parameter(Mandatory)] [string[]] $Arguments,
        [int[]] $AlsoAccept = @()
    )
    & pnputil.exe @Arguments | Out-Host
    $code = $LASTEXITCODE
    if ($code -ne 0 -and $code -ne 3010 -and $AlsoAccept -notcontains $code) {
        throw "pnputil $($Arguments -join ' ') terminó con el código $code."
    }
    return $code
}
