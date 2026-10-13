# Driver de mando virtual de coroom para Windows (`coroomvhid.dll`)

Windows no tiene API de usuario para crear un mando: hace falta un driver. Este es el de coroom: un
**minidriver HID de UMDF2** (el modelo del ejemplo `vhidmini2` de Microsoft y del proyecto
[HIDMaestro](https://hidmaestro.org/), MIT) que corre **en modo usuario**
dentro de `WUDFHost.exe`, debajo del `mshidumdf.sys` que trae Windows. Presenta hasta 8 **mandos
HID genéricos**, uno por ranura de invitado, que ven Steam Input, SDL y DirectInput. `coroom host`
lo usa solo: al dar una ranura (`allow N pad RANURA`) el invitado mueve ese mando y al quitarla (o
al cerrar `coroom`, aunque sea a la fuerza) el mando vuelve al reposo.

Lo que **no** hace falta:

- **Ni firma de prueba ni tocar la seguridad de Windows**: el arranque seguro y la integridad de
  memoria (HVCI) siguen activados y `bcdedit` no se toca. La firma de código en modo kernel no se
  aplica a un driver de modo usuario: los únicos componentes de kernel son `mshidumdf.sys` y
  `WUDFRd.sys`, de Windows y firmados por Microsoft.
- **Ni pagar a Microsoft ni a una autoridad de certificación**: el paquete solo necesita un
  catálogo firmado con un certificado en el que confíe el equipo, y ese certificado lo crea la CI
  en cada compilación y lo instala `install.ps1` (ver «Firma»).
- **Ni código cerrado**: el driver, los scripts y la CI que lo compila y firma están aquí.

Sin el driver, `coroom` en Windows sigue ofreciendo teclado y ratón, y `--gamepad-to-keys`
convierte los mandos en teclas.

## Cómo funciona

```
coroom.exe ──WriteFile (informe de estado)──▶ \\?\HID#VID_0000&PID_0000&Col02#…  (colección de control)
                                                   │ hidclass.sys + mshidumdf.sys (Windows)
                                                   ▼
                                       coroomvhid.dll en WUDFHost.exe (UMDF2, modo usuario)
                                                   │ IOCTL_HID_READ_REPORT
                                                   ▼
                     HID#VID_0000&PID_0000&Col01  «Mando virtual de coroom» ──▶ Steam Input, SDL,
                                                                                 DirectInput, Raw Input
```

- **Un dispositivo raíz por ranura**: `install.ps1` crea `ROOT\COROOMVHID\0000`, `0001`… (clase
  *Dispositivos de interfaz humana*, identificador de hardware `Root\coroomvhid`), cada uno con su
  ranura (0 a 7) en el valor `CoroomSlot` de su clave de hardware. Un driver de UMDF no puede
  crear dispositivos hijos como hacía la versión KMDF sobre Virtual HID Framework, así que los
  mandos existen desde la instalación (ver `-Pads` en «Instalar»).
- **Dos colecciones HID por mando**: el descriptor del mando
  ([`report_descriptor.inc`](report_descriptor.inc), Col01), que es lo que ven los juegos, y una
  colección de control de fabricante ([`control_descriptor.inc`](control_descriptor.inc),
  página `0xFFC0`, Col02) que los juegos ignoran. Por ella `coroom` manda el estado (un informe
  de salida con el informe del mando entero) y lee la ranura y la versión del protocolo (un informe
  de característica). Un minidriver bajo `mshidumdf.sys` no puede exponer un dispositivo de control
  ni IOCTL propios (hidclass manda en lo alto de la pila), así que todo va por informes HID; la
  interfaz está en [`coroom_vhid.h`](coroom_vhid.h) (protocolo 2).
- **Cada ranura es de un programa**: `coroom` abre la colección de control con escritura sin
  compartir, así que otro programa no puede mandar estados a esa ranura a la vez («la ranura la usa
  otro programa»).
- **Perro guardián**: `coroom` repite el estado de cada mando cada 250 ms; si el driver pasa un
  segundo sin noticias (se cerró, murió o se colgó), deja el mando en reposo. Ningún botón se queda
  pulsado.
- **Permisos**: `install.ps1` deja la colección de control solo para SYSTEM, los administradores
  y el usuario con la sesión iniciada en el equipo (`D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)`).
  La colección del mando conserva los permisos de cualquier mando HID, para que los juegos la lean.
- Los tests de `coroom-input` (`crates/input/src/inject/windows_vhid.rs`) leen los dos
  descriptores, la cabecera, el INF y los scripts y comprueban que el cliente de Rust dice lo
  mismo que ellos campo a campo.

### El mando que ven los juegos

Un «Dispositivo de juego compatible con HID» (`Usage Page Generic Desktop`, `Usage Game Pad`),
con el mismo descriptor que la versión KMDF:

| Campo | Contenido | Mando de Xbox del invitado |
|---|---|---|
| Botones 1 a 11 (de 16) | 1 bit cada uno | A, B, X, Y, LB, RB, Back, Start, LS, RS, Guide |
| X, Y | stick izquierdo, −32767 a 32767 (Y positivo hacia abajo) | stick izquierdo |
| Rx, Ry | stick derecho | stick derecho |
| Z, Rz | gatillos, 0 a 32767 | LT, RT |
| Hat | 8 direcciones y centro | cruceta |

Sin vibración (el mando no tiene informes de salida). Nombre: «Mando virtual de coroom»; número de
serie `coroom-pad-N` (N = ranura), para que SDL distinga los mandos entre sí.

| API | ¿Lo ve? |
|---|---|
| DirectInput, Raw Input, HID (`joy.cpl`) | Sí, con 6 ejes, 16 botones y el hat |
| **Steam Input** | Sí, como mando genérico: se configura una vez en Steam (con «Compatibilidad con la configuración de mandos genéricos» activada) y los juegos lanzados desde Steam reciben el mando virtual de Steam (también los de XInput) |
| SDL 2 y 3 | Sí, como *joystick*; para que sea un *gamepad* (el mapeo de botones de Xbox) hace falta un mapeo: el de Steam, o `SDL_GAMECONTROLLERCONFIG` con el GUID que muestra cualquier herramienta de mapeo de SDL |
| Windows.Gaming.Input | Como `RawGameController`, no como `Gamepad` (eso es solo para mandos de Xbox) |
| **Solo XInput** | **No**: XInput solo habla con dispositivos que registran la interfaz XUSB (ver «Lo que falta por verificar»). Con Steam Input, sí |

### Identificadores USB

Los mandos se presentan con **VID 0 y PID 0**. Las alternativas y por qué no:

- **El VID/PID de un mando conocido** (Xbox 360 `045E:028E`, DualShock…): Steam y SDL aplican a
  esos identificadores su driver del protocolo de ese fabricante (HIDAPI de SDL, el modo de Steam
  para mandos de Xbox o de PlayStation), que espera los informes de ese mando y no los de este
  descriptor: el mando dejaría de funcionar justo donde más importa. Además es presentarse como
  un producto de otro.
- **Un PID de [pid.codes](https://pid.codes)** (VID `1209`, gratis para proyectos con licencia de
  código abierto publicada, como este): es lo correcto a medio plazo, pero hay que pedirlo (una
  *pull request* al repositorio de pid.codes) y no se puede usar uno sin asignar. Con un PID propio
  se podría además aportar el mapeo del mando a la base de datos de SDL
  (`SDL_GameControllerDB`) para que sea un *gamepad* sin configurar nada.
- **VID/PID 0**: es lo que hay hoy. Steam Input y SDL reconocen el mando por su uso HID (*Game
  Pad*), no por el VID/PID; SDL lo identifica entonces por su nombre y versión, así que no choca
  con ningún dispositivo real y el mapeo que se guarde una vez vale siempre.

Para cambiar a un PID propio: `COROOM_VHID_VENDOR_ID` y `COROOM_VHID_PRODUCT_ID` en
`coroom_vhid.h` (el cliente encuentra los mandos por la página de uso de la colección de control,
no por el VID/PID, así que no cambia). Los mapeos de SDL y Steam van por identificador: al
cambiarlo hay que rehacerlos.

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

- Windows 11, o Windows 10 22H2; x64 o ARM64. El INF tiene dos variantes de la misma
  instalación: la de Windows 11 usa `MsHidUmdf.inf` (existe desde la compilación 22000), como el
  ejemplo actual de Microsoft; la de Windows 10 añade los servicios `mshidumdf` y `WUDFRd` a
  mano, como el ejemplo anterior a Windows 11 y como HIDMaestro.
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
de Windows 10 2004 a 22H2, la más antigua que se admite) y CRT estática (la DLL no depende del
redistribuible de Visual C++); `build.ps1` pasa InfVerif al INF (la validación básica tiene que
pasar; la de aislamiento del paquete, `/w`, se muestra como aviso porque la variante de Windows 10
añade servicios del sistema a mano). Sin `-Sign` el paquete sale sin firmar y Windows no lo
instala. La CI ([`driver.yml`](../../.github/workflows/driver.yml)) hace lo mismo en cada push y
publica los paquetes con cada versión (`coroomvhid-<versión>-x64.zip` y `-ARM64.zip`).

## Instalar

En una consola de PowerShell abierta como administrador, desde la carpeta del paquete
descomprimido:

```powershell
.\install.ps1            # 4 mandos (ranuras 1 a 4)
.\install.ps1 -Pads 8    # los 8
```

1. Comprueba el certificado del paquete (ver «Firma») y lo instala en «Entidades de
   certificación raíz de confianza» y «Editores de confianza» del equipo.
2. Quita los paquetes `coroomvhid.inf` anteriores (también el KMDF de versiones antiguas, con su
   dispositivo raíz) y los dispositivos que sobran.
3. Crea un dispositivo raíz por ranura y ejecuta `pnputil /add-driver coroomvhid.inf /install`.
4. Limita los permisos de la colección de control de cada mando.

`coroom doctor` debe decir «mandos: driver coroomvhid (protocolo 2, 4 mandos HID genéricos:
ranuras 1, 2, 3, 4)». En el Administrador de dispositivos, «Dispositivos de interfaz humana»,
aparece un «Mando virtual de coroom» por ranura.

**`-Pads`**: los mandos existen siempre, no solo mientras hay invitados, así que los juegos y
Steam los ven (en reposo) aunque nadie los use, y pueden ocupar números de jugador. Instala tantos
como invitados con mando sueles tener; para cambiarlo, vuelve a ejecutar `install.ps1` con otro
valor.

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

Nada de esto se ha probado todavía en un Windows real: la CI solo compila y firma. Para darlo por
bueno, en Windows 11 y en Windows 10 22H2, con el arranque seguro y la integridad de memoria
activados:

1. `install.ps1` sin errores ni avisos y `coroom doctor`.
2. `coroom host --auto-allow pad` y un invitado con mando (`coroom join …`): en `joy.cpl`
   («Configurar dispositivos de juego USB») el «Mando virtual de coroom» de su ranura mueve los
   ejes, los botones y el hat.
3. **Steam**: el mando aparece en *Ajustes → Mando* y se puede configurar como mando genérico; un
   juego lanzado desde Steam con Steam Input lo usa (también uno que solo use XInput).
4. `deny 1 pad` en la consola del anfitrión: su mando vuelve al reposo. `kick 1`, igual.
5. Con un botón pulsado, matar `coroom` (`taskkill /f /im coroom.exe`): en menos de un segundo el
   botón se suelta.
6. Dos `coroom host` a la vez con la misma ranura: el segundo dice que la ranura la usa otro
   programa.
7. `uninstall.ps1` y comprobar que no quedan dispositivos, paquete ni certificados
   (`certlm.msc`).
8. Con el verificador de WDF activado para `coroomvhid.dll` (`WdfVerifier.exe` del WDK),
   repetir 2 a 5 y desactivar y activar un «Mando virtual de coroom» con `coroom` abierto:
   `WUDFHost.exe` no debe terminar.

Anota los resultados (versión de Windows, arquitectura, juegos) en un issue.

## Lo que falta por verificar

- **Todo en un equipo real** (ver la lista de arriba): la CI no carga el driver.
- **Steam con VID/PID 0**: Steam y SDL reconocen los mandos HID genéricos por su uso, pero no se
  ha comprobado con este driver.
- **Smart App Control** (Windows 11): bloquea código sin firma de confianza o sin reputación. No
  se sabe si trata como de confianza una DLL de driver firmada con un certificado que solo es raíz
  de confianza en ese equipo; si la bloquea, el dispositivo no arrancará con Smart App Control
  activado.
- **Anti-trampas en modo kernel**: algunos detectan y rechazan los dispositivos virtuales (este no
  se esconde: es un dispositivo raíz con su nombre).
- **XInput**: un mando HID lo ven Steam, SDL, DirectInput y Raw Input, pero XInput solo habla con
  dispositivos que registran la interfaz **XUSB**. Los juegos que solo usan XInput lo reciben a
  través de Steam Input. Un paso siguiente posible es un dispositivo compañero de UMDF2 que
  registre XUSB y responda a sus IOCTL, como hace HIDMaestro (`HMXInput.dll`); no está hecho.
- **La variante de Windows 10 del INF** y su paso por InfVerif `/w` (se muestra como aviso).
- **Los permisos de la colección de control**: se fijan con SetupAPI después de que hidclass la
  crea; si falla, `install.ps1` avisa y el mando funciona con los permisos por defecto de HID.

## Solución de problemas

| Síntoma | Causa y arreglo |
|---|---|
| `coroom doctor`: «el driver coroomvhid no está instalado o sus mandos están desactivados» | Ejecuta `install.ps1`; en el Administrador de dispositivos, «Dispositivos de interfaz humana», los «Mando virtual de coroom» deben estar activados y sin aviso |
| `install.ps1`: «… no está firmado con coroomvhid.cer» | El paquete está incompleto o mezclado (cada paquete tiene su propio certificado): vuelve a descomprimir el zip de la release |
| `pnputil` rechaza el paquete por la firma, o el dispositivo tiene el código **52** | El certificado del paquete no está en «Entidades de certificación raíz de confianza» y «Editores de confianza» del equipo: vuelve a ejecutar `install.ps1` |
| El dispositivo tiene el código **10** o **31** | `WUDFHost.exe` no pudo cargar el driver: mira el registro de eventos *Microsoft-Windows-DriverFrameworks-UserMode/Operational* (y, con Smart App Control, *CodeIntegrity/Operational*) y anótalo en un issue |
| «la ranura N no tiene mando virtual instalado» | Hay menos mandos que ranuras: `install.ps1 -Pads 8` |
| «el driver coroomvhid instalado habla la versión N» | El driver y `coroom` son de versiones distintas: instala el driver que acompaña a tu `coroom` |
| «la ranura la usa otro programa» | Hay otro `coroom host` abierto con esa ranura |
| «Windows no deja usar el mando» | Solo SYSTEM, administradores y el usuario con la sesión iniciada pueden usarlo: no funciona desde un servicio con otra cuenta |
| El mando se ve en `joy.cpl` pero el juego no lo usa | El juego solo usa XInput: lánzalo desde Steam con Steam Input, o usa `--gamepad-to-keys` |
| Hay mandos «fantasma» en los juegos | Son los mandos instalados que no usa ningún invitado (en reposo): instala menos con `-Pads` |

## Ficheros

| Fichero | Qué es |
|---|---|
| `coroomvhid.c`, `driver.h` | El driver: dispositivo, peticiones HID, estado del mando y perro guardián |
| `coroom_vhid.h` | Interfaz pública (colecciones, informes, ranuras, protocolo); la comprueban los tests de Rust |
| `report_descriptor.inc`, `control_descriptor.inc` | Descriptores HID del mando y de la colección de control, compartidos con el cliente de Rust |
| `coroomvhid.inf` | Paquete: dispositivos `Root\coroomvhid`, clase `HIDClass`, `mshidumdf` y `WUDFRd` de Windows |
| `coroomvhid.vcxproj`, `coroomvhid.sln` | Proyecto de MSBuild con el WDK (UMDF 2.31, como el ejemplo `vhidmini2`) |
| `build.ps1` | Compilar, comprobar el INF, firmar con un certificado de un solo uso y empaquetar |
| `install.ps1`, `uninstall.ps1`, `devices.ps1` | Instalar y desinstalar (sin `devcon` ni `devgen`: los dispositivos raíz se crean con SetupAPI) |
