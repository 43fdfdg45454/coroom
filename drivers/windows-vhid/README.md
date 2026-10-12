# Driver de mando virtual de coroom para Windows (`coroomvhid.sys`)

Windows no tiene API de usuario para crear un mando: hace falta un driver en modo kernel. Este es
el de coroom: un driver KMDF
sobre **Virtual HID Framework** (`vhf.sys`, parte de Windows 10 y 11) que crea bajo demanda hasta
8 **mandos HID genéricos**, uno por ranura de invitado. `coroom host` lo usa solo: al dar una
ranura (`allow N pad RANURA`) aparece un mando nuevo en el sistema y al quitarla (o al cerrar
`coroom`, aunque sea a la fuerza) desaparece.

Sin el driver, `coroom` en Windows sigue ofreciendo teclado y ratón, y `--gamepad-to-keys`
convierte los mandos en teclas.

## Cómo funciona

```
coroom.exe ──IOCTL──▶ \\.\coroomvhid (dispositivo de control, SDDL)
                         │  coroomvhid.sys (KMDF) sobre el dispositivo raíz Root\coroomvhid
                         ▼
                      vhf.sys (filtro inferior) ──▶ mando HID 1 … mando HID 8
                                                       └─ hidclass ─▶ DirectInput, Raw Input, SDL, Steam
```

- `Root\coroomvhid`: dispositivo raíz (clase *Dispositivos del sistema*) que instala
  `coroomvhid.inf`, con `vhf.sys` como filtro inferior. Sus hijos son los mandos.
- `\\.\coroomvhid`: dispositivo de control que atiende las órdenes de `coroom`
  ([`coroom_vhid.h`](coroom_vhid.h)): versión del protocolo, crear y destruir el mando de una
  ranura (0 a 7) y mandarle su estado (un informe HID de 16 bytes). Solo pueden abrirlo SYSTEM,
  los administradores y el usuario con la sesión iniciada en el equipo
  (`D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)`).
- Cada ranura es del handle que la creó: otro programa no puede usarla ni destruirla, y al
  cerrarse el handle se destruyen sus mandos.
- El descriptor HID está en [`report_descriptor.inc`](report_descriptor.inc), compartido con el
  cliente de Rust (`crates/input/src/inject/windows_vhid.rs`), cuyos tests comprueban que el
  informe que empaqueta coincide campo a campo con él y con la cabecera.

### El mando que ven los juegos

Un «Dispositivo de juego compatible con HID» (`Usage Page Generic Desktop`, `Usage Game Pad`):

| Campo | Contenido | Mando de Xbox del invitado |
|---|---|---|
| Botones 1 a 11 (de 16) | 1 bit cada uno | A, B, X, Y, LB, RB, Back, Start, LS, RS, Guide |
| X, Y | stick izquierdo, −32767 a 32767 (Y positivo hacia abajo) | stick izquierdo |
| Rx, Ry | stick derecho | stick derecho |
| Z, Rz | gatillos, 0 a 32767 | LT, RT |
| Hat | 8 direcciones y centro | cruceta |

Sin vibración (no hay informes de salida).

| API | ¿Lo ve? |
|---|---|
| DirectInput, Raw Input, HID (`joy.cpl`) | Sí, con 6 ejes, 16 botones y el hat |
| **Steam Input** | Sí, como mando genérico: se configura una vez en Steam y los juegos lanzados desde Steam reciben el mando virtual de Steam (también los de XInput) |
| SDL 2 y 3 | Sí, como *joystick*; para que sea un *gamepad* (el mapeo de botones de Xbox) hace falta un mapeo: el de Steam, o `SDL_GAMECONTROLLERCONFIG` con el GUID que muestra cualquier herramienta de mapeo de SDL |
| Windows.Gaming.Input | Como `RawGameController`, no como `Gamepad` (eso es solo para mandos de Xbox) |
| **Solo XInput** | **No**: XInput solo habla con mandos de Xbox (driver `xusb22`); emularlos exigiría otro driver de otra clase. Con Steam Input, sí |

### Identificadores USB

Los mandos se presentan con **VID 0 y PID 0**: son virtuales y no hay fabricante ni producto que
declarar. SDL (y Steam Input, que lo usa por debajo) identifica entonces el mando por su nombre y
versión en lugar de por VID/PID, así que no choca con ningún otro dispositivo del equipo y el mapeo
que se guarde una vez vale siempre. Es la opción pensada para equipos propios. Si algún día se
distribuyera el driver firmado, lo correcto sería un PID propio de [pid.codes](https://pid.codes)
(gratis para proyectos con licencia de código abierto publicada, como esta) en `coroom_vhid.h`;
los mapeos de SDL y Steam van por identificador, así que al cambiarlo hay que rehacerlos.

## Requisitos

- Windows 10 1809 (compilación 17763) o posterior, o Windows 11; x64 o ARM64.
- Para compilar: Visual Studio 2022 o 2026 (o sus Build Tools) con «Desarrollo para el escritorio
  con C++», las bibliotecas con mitigación de Spectre y el componente «Windows Driver Kit», más el
  WDK y el SDK de la **misma versión** (10.0.26100 o posterior): instalados, o restaurados como
  paquetes NuGet con `nuget restore packages.config -PackagesDirectory packages` en esta carpeta
  (versión fijada en `packages.config`; `Directory.Build.props` los usa si están). O el **EWDK**
  (todo en una ISO, sin instalar nada).

## Compilar

En PowerShell, desde esta carpeta:

```powershell
.\build.ps1                 # x64 y ARM64, Release, sin firmar
.\build.ps1 -Platform x64 -TestSign   # además, firma de prueba (ver abajo)
```

Con el EWDK, abre antes su entorno (`LaunchBuildEnv.cmd`) y ejecuta `powershell` dentro. El
paquete de cada plataforma queda en `out\x64\` y `out\ARM64\` (`coroomvhid.sys`, `.inf`, `.cat`,
los scripts y este README). Equivale a
`msbuild coroomvhid.sln /p:Configuration=Release /p:Platform=x64`, que es lo que hace la CI
(`.github/workflows/driver.yml`, artefacto `coroomvhid-<plataforma>-desarrollo`: sin firmar y con VID/PID 0,
para equipos propios). El proyecto compila con `/W4 /WX`, KMDF
1.27 (la de Windows 10 1809) e InfVerif.

## Instalar

Windows solo carga drivers de kernel firmados por Microsoft, salvo con la firma de prueba
activada. Hay dos caminos:

### 1. Firma de prueba (desarrollo, tu propio equipo)

1. Desactiva el **arranque seguro** en la UEFI si está activado (con él, Windows ignora la firma
   de prueba).
2. En una consola de administrador: `bcdedit /set testsigning on` y reinicia. En una esquina del
   escritorio aparece «Modo de prueba».
3. `.\build.ps1 -TestSign`: crea un certificado propio en `Cert:\CurrentUser\My`, firma el `.sys`
   y el `.cat` y deja el certificado público en `out\coroomvhid-test.cer` (y en cada paquete).
4. En una consola de administrador: `.\out\x64\install.ps1`. Instala el certificado en
   «Entidades de certificación raíz de confianza» y «Editores de confianza» del equipo, crea el
   dispositivo raíz y ejecuta `pnputil /add-driver coroomvhid.inf /install`.
5. `coroom doctor` debe decir «mandos: driver coroomvhid (protocolo 1, hasta 8 mandos HID
   genéricos)».

No copies el certificado de prueba a otros equipos ni lo uses para otra cosa: quien tenga su
clave privada puede firmar drivers que tu equipo aceptará mientras confíe en él
(`uninstall.ps1 -RemoveTestCertificate` lo quita).

### 2. Firma por atestación (paquetes para distribuir)

Es la firma de Microsoft para drivers que no pasan las pruebas HLK: sirve para Windows 10 y 11 de
escritorio (no para Windows Server) y no requiere la firma de prueba en el equipo del usuario. La
hace quien publica el driver, una vez por versión del driver:

1. **Cuenta**: registro en el programa de hardware de Windows del Partner Center (gratuito) con
   un **certificado EV de firma de código** de una autoridad aceptada por Microsoft. El
   certificado EV es de pago (del orden de varios cientos de euros al año, según la autoridad) y
   va en un token físico o en un HSM en la nube.
2. **PID propio** en `coroom_vhid.h` (ver «Identificadores USB»).
3. `.\build.ps1` (sin `-TestSign`).
4. **CAB**: un `.cab` con una carpeta por arquitectura con el paquete (`coroomvhid.sys`,
   `coroomvhid.inf`, `coroomvhid.pdb`), creado con `makecab /f coroomvhid.ddf` (un `.ddf` que
   liste esos ficheros con `.Set DestinationDir=x64` y `.Set DestinationDir=ARM64`).
5. Firmar el CAB con el certificado EV:
   `signtool sign /fd SHA256 /tr <servidor de sellado de tiempo de la autoridad> /td SHA256 /n "<titular del EV>" coroomvhid.cab`.
6. Partner Center → *Hardware* → *Submit new hardware*: subir el CAB, marcar las arquitecturas y
   «Attestation signing» (sin pruebas HLK). Microsoft devuelve en minutos u horas un ZIP con el
   paquete firmado por «Microsoft Windows Hardware Compatibility Publisher».
7. Ese paquete se instala con `install.ps1` en cualquier Windows 10/11 x64 o ARM64, sin la firma
   de prueba. No hay que subirlo a Windows Update.

## Probar en Windows real

Lo que no se puede comprobar en la CI (que solo compila):

1. Instalar (firma de prueba) y `coroom doctor`.
2. `coroom host --auto-allow pad` y un invitado con mando (`coroom join …`): en
   `joy.cpl` («Configurar dispositivos de juego USB») aparece un «Dispositivo de juego compatible
   con HID» por invitado; en «Propiedades» se ven moverse los ejes, los botones y el hat.
3. `deny 1 pad` en la consola del anfitrión: su mando desaparece de `joy.cpl`. `kick 1`, igual.
4. Con mandos creados, matar `coroom` (`taskkill /f /im coroom.exe`): los mandos desaparecen.
5. Un juego con DirectInput o SDL, y otro desde Steam con Steam Input activado para mandos
   genéricos: cada invitado mueve a su jugador.
6. **Driver Verifier**: en una consola de administrador
   `verifier /standard /driver coroomvhid.sys` y el verificador de KMDF
   `reg add HKLM\SYSTEM\CurrentControlSet\Services\coroomvhid\Parameters\Wdf /v VerifierOn /t REG_DWORD /d 1`;
   reiniciar y repetir 2 a 4, más desactivar y activar «coroom virtual gamepad bus» en el
   Administrador de dispositivos con mandos creados y con `coroom` abierto. Ninguna pantalla
   azul. Para terminar, `verifier /reset` y reiniciar.

Anota los resultados (versión de Windows, arquitectura, juegos) en un issue.

## Desinstalar

En una consola de administrador, con `coroom` cerrado:

```powershell
.\uninstall.ps1 -RemoveTestCertificate
bcdedit /set testsigning off   # si la activaste solo para esto; reinicia después
```

Quita el dispositivo raíz (y con él los mandos), borra el paquete del almacén de drivers
(`pnputil /delete-driver oemNN.inf /uninstall /force`) y, con `-RemoveTestCertificate`, el
certificado de prueba.

## Solución de problemas

| Síntoma | Causa y arreglo |
|---|---|
| `coroom doctor`: «el driver coroomvhid no está instalado o su dispositivo raíz está desactivado» | Ejecuta `install.ps1`; en el Administrador de dispositivos, «Dispositivos del sistema», «coroom virtual gamepad bus» debe estar activado y sin aviso |
| El dispositivo raíz tiene el código **52** (firma) | Paquete con firma de prueba sin `testsigning` (o con arranque seguro activado), o paquete sin firmar: ver «Instalar» |
| Código **10** al volver a activar el dispositivo raíz | Quedaba un `coroom` con el driver abierto: ciérralo y desactiva y activa el dispositivo (o reinicia) |
| «Windows no deja abrir \\\\.\coroomvhid» | Solo SYSTEM, administradores y el usuario con la sesión iniciada pueden usarlo: no funciona desde un servicio con otra cuenta |
| «el driver coroomvhid instalado habla la versión N» | El driver y `coroom` son de versiones distintas: instala el driver que acompaña a tu `coroom` |
| «la ranura la usa otro programa» | Hay otro `coroom host` abierto con esa ranura |
| El mando se ve en `joy.cpl` pero el juego no lo usa | El juego solo usa XInput: lánzalo desde Steam con Steam Input, o usa `--gamepad-to-keys` |
| No carga con la integridad de memoria (HVCI) activada | Mira el registro de eventos *Microsoft-Windows-CodeIntegrity/Operational* y anótalo en un issue |

Los mensajes del driver (`KdPrintEx`, solo en la compilación Debug) se ven con DebugView o
WinDbg con el filtro `IHVDRIVER`.

## Ficheros

| Fichero | Qué es |
|---|---|
| `coroomvhid.c`, `driver.h` | El driver: `DriverEntry`, dispositivo raíz, dispositivo de control, IOCTL, ranuras |
| `coroom_vhid.h` | Interfaz pública (IOCTL, estructuras empaquetadas, disposición del informe); la comprueban los tests de Rust |
| `report_descriptor.inc` | Descriptor HID, compartido con el cliente de Rust |
| `coroomvhid.inf` | Paquete: dispositivo `Root\coroomvhid`, clase `System`, filtro `vhf`, KMDF del sistema sin coinstalador |
| `coroomvhid.vcxproj`, `coroomvhid.sln` | Proyecto de MSBuild con el WDK |
| `build.ps1`, `install.ps1`, `uninstall.ps1`, `devices.ps1` | Compilar, instalar y desinstalar (sin `devcon` ni `devgen`: el dispositivo raíz se crea con SetupAPI) |
