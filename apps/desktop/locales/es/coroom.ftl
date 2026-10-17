# Textos de la aplicación de escritorio de coroom en español.
#
# Formato Fluent (https://projectfluent.org). Cada mensaje se usa con `t!("id")` o
# `t!("id", nombre = valor)` en `apps/desktop/src/ui` (ver `apps/desktop/src/l10n.rs`). Para
# traducir, copia este fichero a `locales/<idioma>/coroom.ftl` y traduce los valores; los
# identificadores y las variables (`{ $nombre }`) no se tocan.

## app

app-ajustes = Ajustes
app-cancelar = Cancelar
app-cerrar = Cerrar
app-cerrar-la-sala-y-salir = ¿Cerrar la sala y salir?
app-cerrar-la-sesion-de-name = ¿Cerrar la sesión de «{ $name }»?
app-cerrar-sesion = Cerrar sesión
app-cerrar-sesion-2 = ¿Cerrar sesión?
app-cerrar-y-salir = Cerrar y salir
app-salir-de-la-sala-y-cerrar = ¿Salir de la sala y cerrar coroom?
app-salir-y-cerrar = Salir y cerrar
app-cerrar-la-sala-y-cambiar-de-servidor = ¿Cerrar la sala y cambiar de servidor?
app-cambiar-de-servidor-cierra-la-sala = Estás compartiendo la pantalla: al cambiar de servidor, la sala se cierra para todos.
app-cerrar-y-cambiar = Cerrar y cambiar
app-compara-estos-numeros-con-name-por = Compara estos números con { $name } por otro medio (en persona o por una llamada). Si coinciden, nadie se ha puesto en medio.
app-compartir = Compartir
app-copiado-al-portapapeles = Copiado al portapapeles
app-dejar-de-compartir = ¿Cerrar la sala?
app-dejar-de-compartir-2 = Cerrar la sala
app-pasar-la-sala-a-name = ¿Pasarle la sala a { $name }?
app-pasar-la-sala-texto = { $name } abrirá una sala nueva (solo voz y chat; desde ella podrá compartir) y todos os pasaréis a ella. Tú entrarás como invitado.
app-pasar-la-sala = Pasar la sala
app-name-te-pasa-la-sala = { $name } te pasa la sala
app-te-pasa-la-sala-texto = Abrirás una sala nueva solo con voz y chat (desde ella podrás compartir tu pantalla) y los demás se pasarán a ella.
app-ahora-no = Ahora no
app-abrir-mi-sala = Abrir mi sala
app-dejaras-de-ver-y-controlar-la = Dejarás de ver y controlar la pantalla del anfitrión. Puedes volver a entrar con el mismo código mientras la sala siga abierta.
app-dejareis-de-ver-si-el-otro = Dejaréis de ver si el otro está conectado y no podréis invitaros a salas. { $name } no recibe ningún aviso; para volver, tendréis que pediros amistad otra vez.
app-ese-dispositivo-queda-desvinculado-de-tu = Ese dispositivo queda desvinculado de tu cuenta: tendrá que volver a iniciar sesión para entrar en salas.
app-estas-compartiendo-la-pantalla-al-salir = Estás compartiendo la pantalla: al salir, la sala se cierra para todos.
app-expulsar = Expulsar
app-expulsar-a-name = ¿Expulsar a { $name }?
app-hay-una-version-nueva = Hay una versión nueva: { $version }
app-huella-de-name-fingerprint = Huella de { $name }: { $fingerprint }
app-inicio = Inicio
app-la-sala-se-cierra-y-todos = La sala se cierra y todos los invitados salen.
app-name-no-puede-dejarte-entrar = { $name } no puede dejarte entrar ahora
app-name-te-deja-entrar = { $name } te deja entrar
app-name-quiere-ser-tu-amigo = { $name } quiere ser tu amigo
app-no-se-pudo-comprobar-err = No se pudo comprobar: { $err }
app-numero-de-seguridad-con-name = Número de seguridad con { $name }
app-quitar = Quitar
app-quitar-a-name-de-tus-amigos = ¿Quitar a { $name } de tus amigos?
app-sale-de-la-sala-al-momento = Sale de la sala al momento y las claves se rotan: no podrá ver nada más. Puede volver a entrar con el código si se lo das otra vez.
app-salir = Salir
app-salir-de-la-sala = ¿Salir de la sala?
app-se-borran-las-credenciales-de-este = Se borran las credenciales de este equipo para este servidor; la identidad del equipo se queda.
app-te-invita-a-su-sala = { $from_name } te invita a su sala { $code }
app-tienes-la-ultima-version = Tienes la última versión
app-volver-al-inicio = Volver al inicio

## Navegación: el riel, la cabecera y los cambios de rol

app-amigos = Amigos
app-sala = Sala
app-cuenta = Cuenta
app-cuenta-de = Cuenta de { $name }
app-destino-atajo = { $destino } · { $atajo }
app-sala-abierta-sin-invitados = Tu sala · todavía sin invitados
app-sala-abierta-un-invitado = Tu sala · 1 invitado
app-sala-abierta-invitados = Tu sala · { $n } invitados
app-abrir-el-diagnostico = Abrir el diagnóstico
app-diagnostico = Diagnóstico
app-sin-microfono = Sin micrófono
app-sin-microfono-detalle = No compartes el micrófono en esta sala
app-microfono-silenciado = Silenciado
app-microfono-abierto = Micrófono abierto
app-activar-tu-microfono = Activar tu micrófono
app-silenciarte = Silenciarte
app-name-pide-entrar = { $name } pide entrar
app-n-piden-entrar = { $n } piden entrar
app-ahora-eres-el-anfitrion = Ahora eres el anfitrión: la sala es tuya.
app-ahora-eres-invitado = Le pasaste la sala a { $name }: ahora eres invitado.

## account

account-sin-sesion = Sin sesión
account-sin-sesion-texto = Inicia sesión en un servidor para ver tu cuenta y los equipos vinculados.
account-perfil = Perfil
account-usuario-en-este-equipo = @{ $username } · en { $device }
account-cerrar-sesion = Cerrar sesión
account-servidor = Servidor
account-servidor-ayuda = Donde está tu cuenta. Su estado, el certificado y el diagnóstico están en Ajustes → Servidor y red.
account-cambiar-de-servidor = Cambiar de servidor
account-dispositivos = Dispositivos de tu cuenta
account-dispositivos-ayuda = Cada equipo con el que inicias sesión. Al desvincular uno, ya no puede entrar en salas hasta que vuelva a iniciar sesión.
account-cargando-los-dispositivos = Cargando los dispositivos de tu cuenta…
account-no-se-pudieron-cargar-los-dispositivos = No se pudieron cargar los dispositivos
account-reintentar = Reintentar
account-este-equipo = Este equipo
account-desvincular = Desvincular
account-ahora-mismo = ahora mismo
account-visto-huella = Visto { $seen } · huella { $short }…
account-numeros-de-seguridad = Números de seguridad
account-numeros-de-seguridad-ayuda = Compáralos en persona o por otro canal: si coinciden, nadie se ha puesto en medio.
account-numeros-de-seguridad-vacio = Aparecen al entrar en una sala o recibir a alguien en la tuya.
account-numero-recortado = { $preview }…
account-ver = Ver
account-detalles-de-seguridad = Detalles de seguridad
account-detalles-de-seguridad-ayuda = La huella completa de la identidad de este equipo, para compararla con la que ven tus amigos.
account-identidad-de-este-equipo = Identidad de este equipo
account-identidad-de-este-equipo-ayuda = Tu clave Ed25519: tus amigos ven esta huella al compartir contigo.
account-copiar = Copiar

## backend

backend-el-servidor-cerro-la-senalizacion = el servidor cerró la señalización
backend-la-prueba-se-detuvo-err = la prueba se detuvo: { $err }
backend-no-se-pudo-abrir-url-err = No se pudo abrir { $url }: { $err }
backend-no-se-pudo-arrancar-una-tarea = no se pudo arrancar una tarea: { $err }
backend-sin-audio-en-la-prueba-local = sin audio en la prueba local
backend-fallo-interno = Fallo interno en «{ $task }»: { $err }. Mira el registro de coroom para enviarlo.
backend-la-sala-no-respondio = El servidor no respondió en { $secs } s al abrir la sala. Comprueba la conexión y vuelve a intentarlo.
backend-sin-conexion-para-pedir-entrar = No hay conexión con el servidor para pedir entrar; vuelve a intentarlo en un momento.
backend-no-se-pudo-pedir-entrar = No se pudo pedir entrar: { $err }


## pantalla-login

pantalla-login-abcd-efgh-jklm = ABCD-EFGH-JKLM
pantalla-login-al-menos-12-caracteres = Al menos 12 caracteres.
pantalla-login-ana = Ana
pantalla-login-anadir-servidor = Añadir servidor
pantalla-login-aparece-en-cuenta-dispositivos = Aparece en Cuenta → Dispositivos, desde donde puedes cerrar su sesión.
pantalla-login-certificado-autofirmado = Certificado autofirmado
pantalla-login-certificado-propio = Certificado propio
pantalla-login-certificado-valido = Certificado válido
pantalla-login-codigo-de-invitacion = Código de invitación
pantalla-login-compara-esta-huella-con-la-que = Compara esta huella con la que te dé quien administra el servidor (la imprime al arrancar) antes de confiar.
pantalla-login-compatible = Compatible
pantalla-login-comprobando = Comprobando { $valor }…
pantalla-login-comprobar = Comprobar
pantalla-login-con-el-codigo-de-invitacion-de = Con el código de invitación de un solo uso que te dio quien administra el servidor (caduca a las 72 horas).
pantalla-login-confiado = Confiado
pantalla-login-confiar = Confiar
pantalla-login-continuar = Continuar
pantalla-login-contrasena = Contraseña
pantalla-login-contrasena-nueva = Contraseña nueva
pantalla-login-creando-la-cuenta = Creando la cuenta…
pantalla-login-crear-cuenta = Crear cuenta
pantalla-login-crear-cuenta-en-host = Crear cuenta en { $host }
pantalla-login-direccion-del-servidor = Dirección del servidor
pantalla-login-el-servidor-de-coroom-de-tu = El servidor de coroom de tu grupo: te da la dirección quien lo administra.
pantalla-login-el-servidor-no-se-ve-por = El servidor no se ve por UDP desde Internet: el relay puede fallar fuera de esta red.
pantalla-login-el-udp-no-llega-desde-esta = El UDP no llega desde esta red: la señalización irá por TCP y la conexión directa puede no funcionar.
pantalla-login-elige-un-servidor = Elige un servidor
pantalla-login-escribe-el-codigo-de-invitacion-que = Escribe el código de invitación que te dieron
pantalla-login-escribe-la-direccion-del-servidor = Escribe la dirección del servidor, por ejemplo https://coroom.example.com
pantalla-login-escribe-tu-contrasena = Escribe tu contraseña
pantalla-login-escribe-tu-nombre-de-usuario = Escribe tu nombre de usuario
pantalla-login-http-3-por-udp = HTTP/3 por UDP
pantalla-login-http-3-por-udp-desde-aqui = HTTP/3 por UDP desde aquí
pantalla-login-iniciando-sesion = Iniciando sesión…
pantalla-login-iniciar-sesion = Iniciar sesión
pantalla-login-iniciar-sesion-en-host = Iniciar sesión en { $host }
pantalla-login-juego-remoto-con-amigos-cifrado-de = Juego remoto con amigos, cifrado de extremo a extremo
pantalla-login-la-conexion-directa-entre-equipos-y = La conexión directa entre equipos y el relay funcionarán desde esta red.
pantalla-login-la-contrasena-nueva-necesita-al-menos = La contraseña nueva necesita al menos 12 caracteres
pantalla-login-la-direccion-no-es-valida-err = La dirección no es válida: { $err }
pantalla-login-las-contrasenas-no-coinciden = Las contraseñas no coinciden
pantalla-login-lo-firma-una-autoridad-en-la = Lo firma una autoridad en la que confía este equipo.
pantalla-login-lo-que-ven-tus-amigos-si = Lo que ven tus amigos; si lo dejas vacío, el de usuario.
pantalla-login-no-se-pudo-conectar-err-comprueba = No se pudo conectar: { $err }. Comprueba la dirección y que el servidor esté en marcha.
pantalla-login-no-se-pudo-recuperar-la-sesion = No se pudo recuperar la sesión guardada: { $err }
pantalla-login-nombre-de-este-dispositivo = Nombre de este dispositivo
pantalla-login-nombre-visible = Nombre visible
pantalla-login-olvidar-servidor = Olvidar servidor
pantalla-login-mas-acciones = Más acciones
pantalla-login-otra-version = Otra versión
pantalla-login-ponle-nombre-a-este-equipo-por = Ponle nombre a este equipo (por ejemplo, «Portátil»)
pantalla-login-protocolo = Protocolo { $protocol }
pantalla-login-recuperando-la-sesion-guardada = Recuperando la sesión guardada…
pantalla-login-reintentar = Reintentar
pantalla-login-repite-la-contrasena = Repite la contraseña
pantalla-login-solo-letras-sin-tilde-cifras-y = Solo letras sin tilde, cifras, «.», «-» y «_» (sin espacios)
pantalla-login-solo-tcp = Solo TCP
pantalla-login-tengo-un-codigo-de-invitacion = Tengo un código de invitación
pantalla-login-usuario = Usuario
pantalla-login-volver-atajo = Volver (Alt+←)
pantalla-login-ya-tengo-cuenta = Ya tengo cuenta

## portal

portal-no-se-pudo-guardar-el-permiso = no se pudo guardar el permiso de pantalla recordado: { $err }
portal-no-se-pudo-olvidar-el-permiso = no se pudo olvidar el permiso de pantalla: { $err }

## runner

runner-error-del-bucle-de-la-ventana = error del bucle de la ventana: { $err }
runner-transmision-en-curso = Transmisión de coroom en curso
runner-este-equipo = este equipo
runner-no-hay-pantalla-disponible-cause-prueba = no hay pantalla disponible ({ $cause }); prueba las órdenes sin ventana (`coroom --help`)
runner-no-se-pudo-abrir-la-ventana = no se pudo abrir la ventana: { $err }
runner-no-se-pudo-capturar-el-cursor = No se pudo capturar el cursor ({ $err }); sigue en modo escritorio
runner-no-se-pudo-crear-la-superficie = no se pudo crear la superficie de la ventana: { $err }
runner-no-se-pudo-preparar-la-gpu = no se pudo preparar la GPU: { $err }
runner-no-se-pudo-preparar-la-ventana = no se pudo preparar la ventana: { $err }

## settings

settings-no-se-pudieron-guardar-los-ajustes = no se pudieron guardar los ajustes: { $err }
settings-no-se-pudieron-leer-los-ajustes = No se pudieron leer los ajustes de { $display } ({ $message }); se usan los de por defecto.
settings-no-se-pudieron-leer-los-ajustes-2 = No se pudieron leer los ajustes de { $display } ({ $err }); se usan los de por defecto.
settings-no-se-pudieron-preparar-los-ajustes = no se pudieron preparar los ajustes: { $err }
settings-no-se-pudo-crear-err = no se pudo crear { $display }: { $err }

## system

system-registro-de-windows = registro de Windows
system-sin-datos-del-sistema = sin datos del sistema
app-host-sin-conexion = { $host } · sin conexión
app-host-rtt-ms = { $host } · { $rtt } ms
app-host-separador = { $host } ·
app-rtt-ms = { $rtt } ms

## Avatares

avatar-titulo = Avatar
avatar-ayuda = Una imagen cuadrada para que los demás te distingan. Se recorta al centro.
avatar-guardando = Guardando…
avatar-elegir = Elegir imagen…
avatar-cambiar = Cambiar imagen…
avatar-quitar = Quitar
avatar-actualizado = Avatar actualizado.
avatar-quitado = Avatar quitado.
avatar-elegir-titulo = Elegir un avatar
avatar-elegir-boton = Elegir
avatar-filtro-imagenes = Imágenes (PNG, JPEG, WebP)
avatar-sin-selector = No hay un selector de ficheros en este sistema.
avatar-no-se-puede-leer = No se puede leer el fichero.
avatar-demasiado-grande = La imagen es demasiado grande: como mucho, 40 MB y 12 000 píxeles de lado.
avatar-formato-no-admitido = Ese formato no vale: elige una imagen PNG, JPEG o WebP.
avatar-danada = La imagen está dañada y no se puede abrir.
avatar-demasiado-pequena = La imagen es demasiado pequeña: al menos 16 píxeles de lado.
avatar-demasiados-cambios = Has cambiado el avatar muchas veces seguidas. Espera un minuto y vuelve a probar.
