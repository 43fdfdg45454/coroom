# devices.ps1: funciones comunes de install.ps1 y uninstall.ps1 (se cargan con ". .\devices.ps1").
#
# Crean y quitan los dispositivos raíz (Root\coroomvhid, el de control, y Root\coroomxusb, los de
# Xbox 360) con SetupAPI, como hace
# "devcon install" / "devcon remove", para no depender de devcon.exe ni devgen.exe (vienen con el
# WDK, no con Windows), y comprueban el certificado con el que está firmado el paquete.

Set-StrictMode -Version Latest

# Los mismos valores que COROOM_VHID_HARDWARE_ID, COROOM_VHID_XUSB_HARDWARE_ID,
# COROOM_VHID_SLOT_VALUE y COROOM_VHID_XUSB_SLOTS de coroom_vhid.h.
$CoroomHardwareId = 'Root\coroomvhid'
$CoroomXusbHardwareId = 'Root\coroomxusb'
$CoroomSlotValue = 'CoroomSlot'
$CoroomXusbSlots = 4
# Clase System (la del INF desde el protocolo 3; antes, HIDClass).
$CoroomClassGuid = [Guid]'4d36e97d-e325-11ce-bfc1-08002be10318'
# Certificado autofirmado con el que la CI (o build.ps1 -Sign) firma cada paquete; su clave privada
# se borra al terminar esa compilación.
$CoroomCertificateSubject = 'CN=coroom (driver local)'
# Certificado de prueba de las versiones anteriores (KMDF): uninstall.ps1 también lo quita.
$CoroomLegacyCertificateSubject = 'CN=coroom test driver signing (NO DISTRIBUIR)'
$CodeSigningOid = '1.3.6.1.5.5.7.3.3'

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

        [StructLayout(LayoutKind.Sequential)]
        struct SP_PROPCHANGE_PARAMS {
            public uint cbSize;          // SP_CLASSINSTALL_HEADER.cbSize
            public uint InstallFunction; // SP_CLASSINSTALL_HEADER.InstallFunction
            public uint StateChange;
            public uint Scope;
            public uint HwProfile;
        }

        const uint DICD_GENERATE_ID = 0x00000001;
        const uint SPDRP_HARDWAREID = 0x00000001;
        const uint SPDRP_SECURITY_SDS = 0x00000018;
        const uint DIF_REMOVE = 0x00000005;
        const uint DIF_PROPERTYCHANGE = 0x00000012;
        const uint DIF_REGISTERDEVICE = 0x00000019;
        const uint DICS_PROPCHANGE = 0x00000003;
        const uint DICS_FLAG_GLOBAL = 0x00000001;
        const uint DIREG_DEV = 0x00000001;
        const uint REG_DWORD = 4;
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

        [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern bool SetupDiGetDeviceInstanceIdW(IntPtr DeviceInfoSet,
            ref SP_DEVINFO_DATA DeviceInfoData, StringBuilder DeviceInstanceId,
            int DeviceInstanceIdSize, out int RequiredSize);

        [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
        static extern IntPtr SetupDiCreateDevRegKeyW(IntPtr DeviceInfoSet,
            ref SP_DEVINFO_DATA DeviceInfoData, uint Scope, uint HwProfile, uint KeyType,
            IntPtr InfHandle, string InfSectionName);

        [DllImport("setupapi.dll", SetLastError = true)]
        static extern bool SetupDiSetClassInstallParamsW(IntPtr DeviceInfoSet,
            ref SP_DEVINFO_DATA DeviceInfoData, ref SP_PROPCHANGE_PARAMS ClassInstallParams,
            int ClassInstallParamsSize);

        [DllImport("setupapi.dll", SetLastError = true)]
        static extern bool SetupDiCallClassInstaller(uint InstallFunction, IntPtr DeviceInfoSet,
            ref SP_DEVINFO_DATA DeviceInfoData);

        [DllImport("setupapi.dll", SetLastError = true)]
        static extern bool SetupDiDestroyDeviceInfoList(IntPtr DeviceInfoSet);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode)]
        static extern int RegSetValueExW(IntPtr hKey, string lpValueName, int Reserved,
            uint dwType, byte[] lpData, int cbData);

        [DllImport("advapi32.dll")]
        static extern int RegCloseKey(IntPtr hKey);

        static Win32Exception LastError(string what) {
            int code = Marshal.GetLastWin32Error();
            return new Win32Exception(code, what + ": " + new Win32Exception(code).Message);
        }

        static SP_DEVINFO_DATA NewData() {
            SP_DEVINFO_DATA data = new SP_DEVINFO_DATA();
            data.cbSize = (uint)Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
            return data;
        }

        // Crea un dispositivo raíz ROOT\<NOMBRE>\NNNN con ese identificador de hardware y el valor
        // DWORD slotValue = slot en su clave de hardware ("Device Parameters"), sin driver:
        // "pnputil /add-driver ... /install" se lo instala después. Devuelve su identificador de
        // instancia.
        public static string CreateRootDevice(Guid classGuid, string deviceName,
                                              string hardwareId, string description,
                                              string slotValue, uint slot) {
            IntPtr set = SetupDiCreateDeviceInfoList(ref classGuid, IntPtr.Zero);
            if (set == INVALID_HANDLE_VALUE) {
                throw LastError("SetupDiCreateDeviceInfoList");
            }
            try {
                SP_DEVINFO_DATA data = NewData();
                if (!SetupDiCreateDeviceInfoW(set, deviceName, ref classGuid, description,
                                              IntPtr.Zero, DICD_GENERATE_ID, ref data)) {
                    throw LastError("SetupDiCreateDeviceInfo");
                }
                // REG_MULTI_SZ: el identificador y dos NUL.
                byte[] ids = Encoding.Unicode.GetBytes(hardwareId + "\0\0");
                if (!SetupDiSetDeviceRegistryPropertyW(set, ref data, SPDRP_HARDWAREID, ids,
                                                       (uint)ids.Length)) {
                    throw LastError("SetupDiSetDeviceRegistryProperty(SPDRP_HARDWAREID)");
                }
                if (!SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, ref data)) {
                    throw LastError("SetupDiCallClassInstaller(DIF_REGISTERDEVICE)");
                }
                try {
                    IntPtr key = SetupDiCreateDevRegKeyW(set, ref data, DICS_FLAG_GLOBAL, 0,
                                                         DIREG_DEV, IntPtr.Zero, null);
                    if (key == INVALID_HANDLE_VALUE) {
                        throw LastError("SetupDiCreateDevRegKey");
                    }
                    try {
                        int status = RegSetValueExW(key, slotValue, 0, REG_DWORD,
                                                    BitConverter.GetBytes(slot), 4);
                        if (status != 0) {
                            throw new Win32Exception(status, "RegSetValueEx(" + slotValue + ")");
                        }
                    } finally {
                        RegCloseKey(key);
                    }
                } catch {
                    // Sin su ranura el driver no arrancaría: no se deja a medias.
                    SetupDiCallClassInstaller(DIF_REMOVE, set, ref data);
                    throw;
                }
                StringBuilder id = new StringBuilder(512);
                int required;
                if (!SetupDiGetDeviceInstanceIdW(set, ref data, id, id.Capacity, out required)) {
                    throw LastError("SetupDiGetDeviceInstanceId");
                }
                return id.ToString();
            } finally {
                SetupDiDestroyDeviceInfoList(set);
            }
        }

        // Quita un dispositivo por su identificador de instancia (ROOT\COROOMVHID\0000).
        public static void RemoveDevice(string instanceId) {
            WithDevice(instanceId, delegate(IntPtr set, ref SP_DEVINFO_DATA data) {
                if (!SetupDiCallClassInstaller(DIF_REMOVE, set, ref data)) {
                    throw LastError("SetupDiCallClassInstaller(DIF_REMOVE)");
                }
            });
        }

        delegate void DeviceAction(IntPtr set, ref SP_DEVINFO_DATA data);

        static void WithDevice(string instanceId, DeviceAction action) {
            IntPtr set = SetupDiCreateDeviceInfoList(IntPtr.Zero, IntPtr.Zero);
            if (set == INVALID_HANDLE_VALUE) {
                throw LastError("SetupDiCreateDeviceInfoList");
            }
            try {
                SP_DEVINFO_DATA data = NewData();
                if (!SetupDiOpenDeviceInfoW(set, instanceId, IntPtr.Zero, 0, ref data)) {
                    throw LastError("SetupDiOpenDeviceInfo(" + instanceId + ")");
                }
                action(set, ref data);
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

# Dispositivos de coroom (presentes o no): el de control, los de Xbox 360 y los mandos raíz de las
# versiones anteriores (también Root\coroomvhid, de clase HIDClass).
function Get-CoroomDevice {
    Get-PnpDevice -ErrorAction SilentlyContinue |
        Where-Object { $_.HardwareID -contains $CoroomHardwareId -or $_.HardwareID -contains $CoroomXusbHardwareId }
}

# Paquetes del driver en el almacén de drivers (oemNN.inf).
function Get-CoroomDriverPackage {
    Get-WindowsDriver -Online -ErrorAction SilentlyContinue |
        Where-Object { (Split-Path -Leaf $_.OriginalFileName) -ieq 'coroomvhid.inf' }
}

# Certificados de los paquetes de coroom en un almacén del equipo (por sujeto o por huella).
function Get-CoroomCertificate {
    param(
        [Parameter(Mandatory)] [string] $Store,
        [string[]] $Thumbprint = @()
    )
    Get-ChildItem $Store | Where-Object {
        $_.Subject -eq $CoroomCertificateSubject -or
        $_.Subject -eq $CoroomLegacyCertificateSubject -or
        $Thumbprint -contains $_.Thumbprint
    }
}

# Comprueba que el certificado del paquete es el que firmó el .cat y el .dll, y que solo sirve
# para firmar código; devuelve el certificado. Así install.ps1 solo confía en lo que firmó este
# paquete.
function Assert-CoroomPackageSignature {
    param([Parameter(Mandatory)] [string] $PackageDir)
    $cerPath = Join-Path $PackageDir 'coroomvhid.cer'
    if (-not (Test-Path $cerPath)) {
        throw "El paquete no trae coroomvhid.cer (el certificado con el que está firmado). Usa un paquete de la CI o compila con .\build.ps1 -Sign."
    }
    $certificate = [Security.Cryptography.X509Certificates.X509Certificate2]::new($cerPath)
    if ($certificate.HasPrivateKey) {
        throw 'coroomvhid.cer trae una clave privada: no es un paquete de coroom. No se instala.'
    }
    if ($certificate.Subject -ne $CoroomCertificateSubject -or $certificate.Subject -ne $certificate.Issuer) {
        throw "coroomvhid.cer no es el certificado autofirmado de coroom («$($certificate.Subject)»). No se instala."
    }
    $eku = @($certificate.Extensions | Where-Object { $_ -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension] })
    $usages = @($eku | ForEach-Object { $_.EnhancedKeyUsages } | ForEach-Object { $_.Value })
    if ($usages.Count -ne 1 -or $usages[0] -ne $CodeSigningOid) {
        throw "coroomvhid.cer debe servir solo para firmar código (EKU $CodeSigningOid) y sirve para: $($usages -join ', '). No se instala."
    }
    foreach ($file in 'coroomvhid.cat', 'coroomvhid.dll') {
        $path = Join-Path $PackageDir $file
        if (-not (Test-Path $path)) {
            throw "Falta $file en el paquete."
        }
        $signature = Get-AuthenticodeSignature -FilePath $path
        if (-not $signature.SignerCertificate -or $signature.SignerCertificate.Thumbprint -ne $certificate.Thumbprint) {
            throw "$file no está firmado con coroomvhid.cer ($($signature.StatusMessage)). No se instala."
        }
    }
    return $certificate
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
