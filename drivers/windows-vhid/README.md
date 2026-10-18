# Driver de mandos virtuales de coroom para Windows (`coroomvhid.dll`)

Windows no tiene API de usuario para crear un mando: hace falta un driver. Este es el de coroom, un
driver de **UMDF2** que corre **en modo usuario** dentro de `WUDFHost.exe`, con dos clases de
dispositivo:

- **Mandos tal cual**: el mando del invitado recreado en el anfitrión con su VID/PID, su versión y
  su descriptor HID, como fuente de la **Virtual HID Framework** (`vhf.sys`, de Windows) que crea y
  quita mandos al momento. Steam, SDL y los juegos lo reconocen como ese mando (DualSense, Switch
  Pro, 8BitDo…), con sus botones extra, y la vibración y las luces vuelven al mando del invitado.
- **Mandos de Xbox 360** (cuatro, para XInput): los mandos **convertidos** van a ellos, así que los
  ven también los juegos que solo usan XInput, y un mando de Xbox por cable del invitado llega con
  su VID/PID. Responden a los IOCTL de `xusb22` como un mando por cable, y la vibración que les
  piden los juegos (`XInputSetState`) vuelve al mando del invitado; las disposiciones, que
  Microsoft no documenta, siguen a [HIDMaestro](https://hidmaestro.org/) (MIT).

`coroom host` lo usa solo: al dar una ranura (`allow N pad RANURA`) el invitado tiene su mando, y
al quitarla (o al cerrar `coroom`, aunque sea a la fuerza) el mando desaparece.

Lo que **no** hace falta:

- **Ni firma de prueba ni tocar la seguridad de Windows**: el arranque seguro y la integridad de
  memoria (HVCI) siguen activados y `bcdedit` no se toca. La firma de código en modo kernel no se
  aplica a un driver de modo usuario: los únicos componentes de kernel son `WUDFRd.sys` y
  `vhf.sys`, de Windows y firmados por Microsoft.
- **Ni pagar a Microsoft ni a una autoridad de certificación**: el paquete solo necesita un
  catálogo firmado con un certificado en el que confíe el equipo, y ese certificado lo crea la CI
  en cada compilación y lo instala `install.ps1` (ver «Firma»).
- **Ni código cerrado**: el driver, los scripts y la CI que lo compila y firma están aquí.

Sin el driver, `coroom` en Windows sigue ofreciendo teclado y ratón, y `--gamepad-to-keys`
convierte los mandos en teclas.

## Cómo funciona

```
coroom.exe ──DeviceIoControl──▶ Root\coroomvhid  «Mandos virtuales de coroom» (exclusivo)
                │                  coroomvhid.dll en WUDFHost.exe, sobre vhf.sys
                │                      │ VhfCreate (VID/PID y descriptor del invitado)
                │                      ▼
                │                  HID\VID_2DC8&PID_6012…  el mando tal cual ──▶ Steam, SDL,
                │                                                                 DirectInput, Raw Input
                └─DeviceIoControl──▶ Root\coroomxusb ×4  «Mando de Xbox 360 de coroom»
                                   interfaz XUSB (solo mientras está activo) ──▶ XInput,
                                                                                 Windows.Gaming.Input
```

- **Dispositivo de control** (`Root\coroomvhid`, uno, clase *Dispositivos del sistema*): `coroom`
  lo abre (es exclusivo: un programa a la vez) y por su interfaz privada pide mandos
  (`COROOM_VHID_IOCTL_CREATE`, con VID/PID, versión, descriptor, informe de reposo e informes de
  característica), les manda informes de entrada y recoge las salidas (vibración, luces) que el
  sistema les escribe. Cada mando es un dispositivo VHF hijo, con su propio *container ID*. Al
  cerrar el handle (o morir `coroom`) el driver quita todos sus mandos.
- **Mandos de Xbox 360** (`Root\coroomxusb`, cuatro, cada uno con su ranura en `CoroomSlot`): su
  interfaz XUSB solo está activa mientras `coroom` los usa, así que los juegos no ven mandos
  fantasma. El que los activa es su dueño: otro programa no puede mandarles estados ni leer su
  vibración. Cada uno guarda la última vibración que le pidió un juego (`IOCTL_XUSB_SET_STATE`)
  hasta que `coroom` la lee (`COROOM_VHID_IOCTL_XUSB_RUMBLE`, cada pocos ms) y la manda al
  invitado de la ranura.
- **Qué va a cada uno**: un mando convertido, al primer Xbox 360 libre y, si no queda ninguno, a un
  mando HID genérico ([`report_descriptor.inc`](report_descriptor.inc)); un mando HID tal cual, a
  un mando VHF con su identidad; un Xbox por cable tal cual, a un Xbox 360 con su VID/PID.
- **Características**: Windows lee los informes de característica del mando al enumerarlo (la
  calibración de un DualSense, por ejemplo); el driver contesta con los que el invitado mandó al
  conectar, sin ida y vuelta por la red.
- **Perro guardián**: `coroom` repite el estado de los mandos convertidos cada 250 ms; si el
  driver pasa un segundo sin noticias, los deja en reposo. Los mandos tal cual los repite el
  invitado cada 100 ms.
- **Permisos** (en el INF): el dispositivo de control, solo SYSTEM, administradores y el usuario
  con la sesión iniciada (`D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)`); los Xbox 360, cualquiera
  (los abren los juegos), pero sus órdenes privadas solo valen para quien los activó.
- La interfaz está en [`coroom_vhid.h`](coroom_vhid.h) (protocolo 4; el 4 añade la vibración de
  los Xbox 360). Los tests de
  `coroom-input` (`crates/input/src/inject/windows_vhid.rs`) leen la cabecera, el descriptor, el INF
  y los scripts y comprueban que el cliente de Rust dice lo mismo campo a campo.

### Qué ven los juegos

| Mando | DirectInput, Raw Input, HID | SDL y Steam | XInput (juegos que solo usan XInput) |
|---|---|---|---|
| Convertido (Xbox 360) | Como mando de Xbox 360 | Sí, como Xbox 360 | **Sí** |
| Tal cual (HID) | Sí, con su descriptor | Sí, como ese mando (DualSense, Switch Pro…) | A través de Steam Input |
| Xbox por cable, tal cual | Como mando de Xbox 360, con su VID/PID | Sí | **Sí** |
| Genérico (sin Xbox 360 libres) | Sí: 6 ejes, 16 botones y hat | Como *joystick* (con Steam Input, como mando) | A través de Steam Input |

Los mandos tal cual se presentan siempre como **USB**, aunque el del invitado vaya por Bluetooth
(SDL decide el modo USB o Bluetooth de un DualSense o un Switch Pro por el bus). La vibración de
los Xbox 360 vuelve al invitado como el estado de sus dos motores, y allí la aplica a su mando
(sea cual sea): con su nodo `evdev` en Linux y con `gilrs` en Windows.

## Firma

Windows instala un paquete de drivers si su catálogo (`coroomvhid.cat`) está firmado con un
certificado que encadena a una raíz de confianza del equipo; si además está en «Editores de
confianza», lo instala sin preguntar. El requisito de que Microsoft firme el driver (atestación o
WHQL) solo se aplica al código de modo kernel, y `coroomvhid.dll` es de modo usuario.

En cada compilación, la CI ([`.github/workflows/driver.yml`](../../.github/workflows/driver.yml),
con `build.ps1 -Sign`):

1. crea un certificado autofirmado **nuevo** en el almacén del usuario del runner:
   `CN=coroom (driver local)`, **solo para firmar código** (EKU `1.3.6.1.5.5.7.3.3`), RSA 3072,
   SHA-256, con la clave privada **no exportable**;
2. firma `coroomvhid.dll`, crea el catálogo con Inf2Cat y lo firma, las dos firmas con **sello de
   tiempo RFC 3161** (siguen siendo válidas cuando caduque el certificado);
3. deja en el paquete **solo la parte pública**, `coroomvhid.cer`;
4. **borra el certificado y su clave privada** del runner y comprueba que no queda nada y que el
   paquete no lleva claves.

La clave existe solo durante esa ejecución de la CI: nadie (tampoco quien mantiene coroom) puede
firmar después otro código con el certificado en el que confía quien instala el paquete. Cada
paquete tiene su propio certificado; `install.ps1` comprueba que el `.cer` es el que firmó ese
`.cat` y esa `.dll` y que solo sirve para firmar código antes de instalarlo en «Entidades de
certificación raíz de confianza» y «Editores de confianza» del equipo, y `uninstall.ps1` lo
quita. Para auditarlo: la ejecución de la CI que lo creó muestra su huella en el registro.

Se puede repetir en local con `.\build.ps1 -Sign`, que hace lo mismo con un certificado propio de
un solo uso.

## Requisitos

- Windows 11, o Windows 10 22H2; x64 o ARM64. Una sola instalación para los dos: `WUDFRd` como
  servicio del dispositivo y `vhf` como filtro inferior del de control, los dos de Windows.
- Para compilar: Visual Studio 2022 o 2026 (o sus Build Tools) con «Desarrollo para el escritorio
  con C++», las bibliotecas con mitigación de Spectre y el componente «Windows Driver Kit», más el
  WDK y el SDK de la **misma versión** (10.0.26100 o posterior): instalados, o restaurados como
  paquetes NuGet con `nuget restore packages.config -PackagesDirectory packages` en esta carpeta
  (versión fijada en `packages.config`; `Directory.Build.props` los usa si están). O el **EWDK**
  (todo en una ISO, sin instalar nada).

## Compilar

En PowerShell, desde esta carpeta:

```powershell
.\build.ps1 -Sign                         # x64 y ARM64, Release, firmado con un certificado de un solo uso
.\build.ps1 -Platform x64 -Sign -Version 1.4.0   # DriverVer 1.4.0.0
```

Con el EWDK, abre antes su entorno (`LaunchBuildEnv.cmd`) y ejecuta `powershell` dentro. El
paquete de cada plataforma queda en `out\x64\` y `out\ARM64\` (`coroomvhid.dll`, `.inf`, `.cat`,
`.cer`, los scripts, la cabecera y este README). El proyecto compila con `/W4 /WX`, UMDF 2.31 (la
de Windows 10 2004 a 22H2, la más antigua que se admite), `VhfUm.lib` y CRT estática (la DLL no
depende del redistribuible de Visual C++); `build.ps1` pasa InfVerif al INF (la validación básica
tiene que pasar; la de aislamiento del paquete, `/w`, se muestra como aviso porque el INF añade el
servicio `WUDFRd` a mano). Sin `-Sign` el paquete sale sin firmar y Windows no lo instala. La CI
([`driver.yml`](../../.github/workflows/driver.yml)) hace lo mismo en cada push y publica los
paquetes con cada versión (`coroomvhid-<versión>-x64.zip` y `-ARM64.zip`).

## Instalar

En una consola de PowerShell abierta como administrador, desde la carpeta del paquete
descomprimido:

```powershell
.\install.ps1
```

1. Comprueba el certificado del paquete (ver «Firma») y lo instala en «Entidades de
   certificación raíz de confianza» y «Editores de confianza» del equipo.
2. Quita los paquetes `coroomvhid.inf` anteriores y los dispositivos de coroom que hubiera
   (también los mandos raíz de las versiones anteriores, de clase *Dispositivos de interfaz
   humana*).
3. Crea el dispositivo de control y los cuatro Xbox 360 y ejecuta
   `pnputil /add-driver coroomvhid.inf /install`.

`coroom doctor` debe decir «mandos: driver coroomvhid (protocolo 4): mandos tal cual (HID) y 4
mandos de Xbox 360». Un `coroom` nuevo con el driver de antes (protocolo 3) lo dice y no usa los
mandos: **al actualizar coroom, vuelve a ejecutar `install.ps1`** con el paquete del driver de
esa versión. En el Administrador de dispositivos, «Dispositivos del sistema», aparecen
«Mandos virtuales de coroom» y cuatro «Mando de Xbox 360 de coroom»; los mandos de los invitados
aparecen y desaparecen mientras `coroom` comparte. **Al actualizar desde un driver del protocolo 2
(un mando raíz por ranura), vuelve a ejecutar `install.ps1`**: quita los mandos antiguos.

## Desinstalar

En una consola de administrador, con `coroom` cerrado:

```powershell
.\uninstall.ps1
```

Quita los dispositivos (y con ellos los mandos), borra el paquete del almacén de drivers
(`pnputil /delete-driver oemNN.inf /uninstall /force`) y quita de los almacenes del equipo los
certificados de coroom (`CN=coroom (driver local)`, el de este paquete por huella y el de prueba de
versiones antiguas). No hay nada más que deshacer.

## Probar en Windows real

La CI (job «Prueba del driver instalado» de
[`driver.yml`](../../.github/workflows/driver.yml), antes de cada publicación) instala el paquete
x64 en un runner de Windows Server 2025 con `install.ps1`, comprueba que el dispositivo de control
y los cuatro Xbox 360 arrancan y ejecuta
[`crates/input/tests/windows_driver.rs`](../../crates/input/tests/windows_driver.rs) con el
cliente de coroom: un segundo cliente no puede abrir el driver; un convertido es un Xbox 360 que
XInput ve con los botones que se mandan y cuya vibración (`XInputSetState`) vuelve al cliente; un
tal cual aparece como HID con su VID/PID, contesta sus
características con las del invitado, entrega sus informes y devuelve las salidas que le escribe
Windows; sin Xbox 360 libres sale el genérico; al cerrar no queda ningún mando. Después,
`uninstall.ps1` y comprueba que no quedan dispositivos, paquete ni certificados.

Lo que la CI no ve (sin juegos, sin Steam, sin mandos físicos, sin Windows de escritorio), en
Windows 11 y en Windows 10 22H2, con el arranque seguro y la integridad de memoria activados:

1. `install.ps1` sin errores ni avisos y `coroom doctor`.
2. **Convertido**: `coroom host --auto-allow pad` y un invitado con mando, con «Convertido a Xbox
   360»: en `joy.cpl` aparece un mando de Xbox 360 que mueve ejes, botones y cruceta; un juego que
   solo use XInput lo usa sin Steam, y su vibración llega al mando del invitado (también con un
   juego de Windows.Gaming.Input).
3. **Tal cual**: un invitado con un DualSense, un Switch Pro o un 8BitDo, en «Tal cual»: en
   `joy.cpl` aparece con su nombre; Steam lo reconoce como ese mando (*Ajustes → Mando*), con sus
   botones extra; la vibración de un juego llega al mando del invitado.
4. Un invitado con un mando de Xbox por cable, en «Tal cual»: XInput lo ve con su VID/PID.
5. `deny 1 pad`: su mando desaparece. `kick 1`, igual. Con un botón pulsado, matar `coroom`
   (`taskkill /f /im coroom.exe`): los mandos desaparecen y nada se queda pulsado.
6. Dos `coroom host` a la vez: el segundo dice que el driver lo usa otro programa.
7. `uninstall.ps1` y comprobar que no quedan dispositivos, paquete ni certificados
   (`certlm.msc`).
8. Con el verificador de WDF activado para `coroomvhid.dll` (`WdfVerifier.exe` del WDK),
   repetir 2 a 5 y desactivar y activar el dispositivo de control con `coroom` abierto:
   `WUDFHost.exe` no debe terminar.

Anota los resultados (versión de Windows, arquitectura, mandos, juegos) en un issue.

## Lo que falta por verificar

- **En un equipo de escritorio** (la lista de arriba): la CI lo prueba en Windows Server 2025 x64,
  sin juegos ni Steam; ARM64 solo se compila.
- **Los IOCTL de `xusb22`**: no están documentados; se siguen los valores de HIDMaestro. XInput los
  acepta en la CI (estado, botones y la vibración de `XInputSetState`); la vibración que manda
  Windows.Gaming.Input, sin probar.
- **Mandos que preguntan al PC**: un Switch Pro manda subcomandos y espera respuesta por informes
  de entrada; hoy esas preguntas llegan al mando del invitado como salidas y su respuesta vuelve
  como un informe más, con la latencia de la red. Si algún mando no lo tolera, irá convertido.
- **Smart App Control** (Windows 11): bloquea código sin firma de confianza o sin reputación. No
  se sabe si trata como de confianza una DLL de driver firmada con un certificado que solo es raíz
  de confianza en ese equipo; si la bloquea, los dispositivos no arrancarán con Smart App Control
  activado.
- **Anti-trampas en modo kernel**: algunos detectan y rechazan los dispositivos virtuales (estos no
  se esconden: son dispositivos raíz con su nombre).
- **El INF** y su paso por InfVerif `/w` (se muestra como aviso).

## Solución de problemas

| Síntoma | Causa y arreglo |
|---|---|
| `coroom doctor`: «el driver coroomvhid no está instalado o está desactivado» | Ejecuta `install.ps1`; en el Administrador de dispositivos, «Dispositivos del sistema», «Mandos virtuales de coroom» debe estar activado y sin aviso |
| `install.ps1`: «… no está firmado con coroomvhid.cer» | El paquete está incompleto o mezclado (cada paquete tiene su propio certificado): vuelve a descomprimir el zip de la release |
| `pnputil` rechaza el paquete por la firma, o el dispositivo tiene el código **52** | El certificado del paquete no está en «Entidades de certificación raíz de confianza» y «Editores de confianza» del equipo: vuelve a ejecutar `install.ps1` |
| El dispositivo tiene el código **10** o **31** | `WUDFHost.exe` no pudo cargar el driver: mira el registro de eventos *Microsoft-Windows-DriverFrameworks-UserMode/Operational* (y, con Smart App Control, *CodeIntegrity/Operational*) y anótalo en un issue |
| «el driver coroomvhid instalado habla la versión N» | El driver y `coroom` son de versiones distintas: instala el driver que acompaña a tu `coroom` |
| «lo está usando otro programa» | Hay otro `coroom host` abierto (el dispositivo de control es de un programa a la vez) |
| «Windows no aceptó el descriptor del mando» | El mando del invitado va convertido: anota el mando (`coroom doctor` en el invitado) en un issue |
| «no queda ningún mando de Xbox 360 libre» | Hay más de cuatro mandos de Xbox por cable tal cual: los demás van convertidos |
| El mando se ve en `joy.cpl` pero el juego no lo usa | El juego solo usa XInput y el mando va tal cual (HID): ponlo en «Convertido a Xbox 360» o lanza el juego desde Steam con Steam Input |

## Ficheros

| Fichero | Qué es |
|---|---|
| `coroomvhid.c`, `driver.h` | El driver: dispositivo de control (mandos VHF, entradas, salidas, características) y Xbox 360 (IOCTL de `xusb22`), con sus perros guardianes |
| `coroom_vhid.h` | Interfaz pública (IOCTL, estructuras, límites, protocolo); la comprueban los tests de Rust |
| `report_descriptor.inc` | Descriptor del mando genérico, compartido con el cliente de Rust |
| `coroomvhid.inf` | Paquete: `Root\coroomvhid` (sobre `vhf`) y `Root\coroomxusb`, clase *System*, `WUDFRd` de Windows |
| `coroomvhid.vcxproj`, `coroomvhid.sln` | Proyecto de MSBuild con el WDK (UMDF 2.31 y `VhfUm.lib`) |
| `build.ps1` | Compilar, comprobar el INF, firmar con un certificado de un solo uso y empaquetar |
| `install.ps1`, `uninstall.ps1`, `devices.ps1` | Instalar y desinstalar (sin `devcon` ni `devgen`: los dispositivos raíz se crean con SetupAPI) |
