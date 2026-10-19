# coroom para Android (invitado)

La aplicación de Android solo **entra** en salas: no comparte la pantalla. La interfaz está en
Kotlin con Jetpack Compose (Material 3, con el color dinámico del sistema) y toda la lógica
(cuenta, amigos, sala, vídeo con MediaCodec, audio con AAudio, cifrado) está en Rust, en
`apps/android`, que se compila a `libcoroom_android.so` y se carga por JNI (`Native.kt`).

```
packaging/android/
├── settings.gradle.kts, build.gradle.kts, gradle.properties
├── gradle/libs.versions.toml          versiones fijadas
├── gradle/wrapper/gradle-wrapper.properties   Gradle con su sha256 (sin el .jar)
└── app/
    ├── build.gradle.kts               versionCode, firma, tarea cargoBuild
    └── src/main/
        ├── AndroidManifest.xml
        ├── kotlin/coroom/android/
        │   ├── Native.kt              el puente JNI
        │   ├── bridge/                llamadas JSON y eventos de Rust
        │   ├── platform/              Keystore, red, mandos, vibración, decodificadores, avisos
        │   ├── settings/              ajustes (DataStore)
        │   └── ui/                    pantallas (main/ y room/), componentes y tema
        ├── jniLibs/                   lo deja cargo-ndk (no va al repositorio)
        └── res/                       textos en español, iconos y temas
```

## Requisitos

- **JDK 17** o más nuevo (el de Android Studio sirve).
- **Android SDK** con la plataforma **API 37** y las build-tools que pida el plugin de Android
  (Android Studio las instala solas; sin él, `sdkmanager "platforms;android-37"`). La ruta va en
  `ANDROID_HOME` o en `packaging/android/local.properties` (`sdk.dir=…`, no va al repositorio).
- **Android NDK** reciente (`sdkmanager --list | grep ndk` y la más nueva estable), con
  `ANDROID_NDK_HOME` apuntando a él.
- **Rust** con los objetivos de Android y **cargo-ndk**:

  ```bash
  rustup target add aarch64-linux-android x86_64-linux-android
  cargo install cargo-ndk --locked
  ```

- **Gradle 9.8.1**: el repositorio lleva `gradle-wrapper.properties` con la suma sha256 de la
  distribución, pero no el `.jar` ni los scripts `gradlew`. Para crearlos una vez, con un Gradle
  instalado:

  ```bash
  cd packaging/android
  gradle wrapper --gradle-version 9.8.1 \
    --gradle-distribution-sha256-sum dce76f55f8e251a3a1f130eb120f30b3d271de2b76c9b0729d316b5a1b6dc01f
  ```

  Después se usa `./gradlew`. También vale el Gradle instalado directamente (`gradle …`).

## Compilar

Primero la biblioteca de Rust, desde la raíz del repositorio:

```bash
cargo ndk -t arm64-v8a -t x86_64 -P 28 --link-libcxx-shared \
  -o packaging/android/app/src/main/jniLibs build --release -p coroom-android
```

`arm64-v8a` es la de los teléfonos; `x86_64`, la del emulador. Luego el APK:

```bash
cd packaging/android
./gradlew assembleDebug          # app/build/outputs/apk/debug/app-debug.apk
./gradlew assembleRelease        # app/build/outputs/apk/release/app-release.apk
```

Con `-Pcoroom.cargo=true`, Gradle ejecuta el `cargo ndk` de arriba antes de cada build
(tarea `cargoBuild`), así no hace falta lanzarlo a mano:

```bash
./gradlew -Pcoroom.cargo=true assembleDebug
```

### Versión

La versión sale de `coroom.version` (`1.0.0` en `gradle.properties`); la CI pasa la de
GitVersion con `-Pcoroom.version=X.Y.Z`. No se edita a mano. El `versionCode` es
`X × 1 000 000 + Y × 1 000 + Z`, así que cada parte va de 0 a 999.

### Firma de la release

La release se firma con un almacén de claves que **no** está en el repositorio. Se pasa por
variables de entorno:

| Variable                           | Qué es                                  |
|------------------------------------|-----------------------------------------|
| `COROOM_ANDROID_KEYSTORE`          | ruta al `.jks` o `.p12`                 |
| `COROOM_ANDROID_KEYSTORE_PASSWORD` | contraseña del almacén                  |
| `COROOM_ANDROID_KEY_ALIAS`         | alias de la clave                       |
| `COROOM_ANDROID_KEY_PASSWORD`      | contraseña de la clave                  |

Sin ellas, `assembleRelease` firma con la clave de depuración (sirve para probar, no para
publicar). Para crear un almacén propio:

```bash
keytool -genkeypair -v -keystore coroom-release.jks -alias coroom \
  -keyalg EC -groupname secp256r1 -validity 10000
```

## Instalar y probar

Con el teléfono en modo desarrollador y la depuración USB (o inalámbrica) activada:

```bash
adb devices
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb logcat -s coroom            # el registro de la parte en Rust
```

El mismo registro, con la hora de cada línea y el nivel `info` de coroom (`warn,coroom=info`), se
guarda en el directorio privado de la aplicación (`files/logs/coroom.log`; la ejecución anterior en
`coroom.1.log`, y rota a los 10 MB). Sin cable: Ajustes → Ayuda → «Compartir registro» (o el botón
del panel «Conexión y calidad» de la sala) manda los dos ficheros por la hoja de compartir del
sistema.

La primera vez se pone la dirección del servidor (por ejemplo `https://coroom.example.com:4433`).
Si el certificado es de una CA propia, se puede instalar en el teléfono (Ajustes → Seguridad →
Cifrado y credenciales → Instalar certificado → Certificado de CA) o aceptar la huella SHA-256 que
enseña la aplicación, comparándola con la que da el operador del servidor.

Con el servidor de desarrollo (`cargo run -p coroom-server`) en el mismo equipo, el emulador lo
alcanza en `https://10.0.2.2:4433`. Un teléfono tiene que llegar al equipo por la LAN: `adb
reverse` solo reenvía TCP y la sala va por QUIC (UDP).

## Qué hace la parte de Kotlin

- **Sesión**: la clave que cifra la sesión guardada vive en el Android Keystore (AES-256-GCM, no
  sale del teléfono ni va en copias de seguridad).
- **Red**: avisa a Rust de la puerta de enlace y la MTU de la red por defecto (`set_route`) y le
  pasa las CA del sistema y del usuario (`set_roots`). No hay servicios de notificaciones push:
  los avisos de invitación llegan mientras la aplicación sigue abierta en segundo plano.
- **Sala**: en horizontal, a pantalla completa y con la pantalla encendida. Pasa los toques del
  vídeo, el teclado (físico o en pantalla, con el texto que no es ninguna tecla, como ñ, á o
  emoji, como texto) y los mandos; un ratón o un touchpad conectado captura el puntero con un clic
  en el vídeo y manda movimiento relativo, botones y rueda (lo suelta al abrir un panel o salir).
  Dibuja el cursor del anfitrión y hace vibrar el mando cuando lo pide el juego.
- **Avatares**: los de la cuenta, los amigos y la gente de la sala, de la caché de disco que
  mantiene Rust (`avatar`); mientras llegan, la inicial.
- **Decodificadores**: manda a Rust la lista de MediaCodec (H.264, HEVC, AV1) antes de entrar.

## Licencias

El código es Apache-2.0, como el resto del cliente. Los iconos de la interfaz son de
[Lucide](https://lucide.dev) (ISC; el aviso va en la aplicación, en Ajustes → Licencias, y en
`app/src/main/res/raw/lucide_license.txt`). El icono de la aplicación es el de
`packaging/icons/coroom.svg`, dibujo propio del proyecto.
