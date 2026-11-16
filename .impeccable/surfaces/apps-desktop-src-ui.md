---
version: 1
slug: "apps-desktop-src-ui"
primary_target: "apps/desktop/src/ui"
related_targets: ["packaging/android","crates/ui"]
---

# Aplicación de escritorio y invitado Android

Alcance: toda la interfaz de `apps/desktop/src/ui` (con el sistema de diseño `crates/ui`) y la del
invitado Android (`packaging/android`). Modo del visitante: **Operate** (pantallas de trabajo).

Quién y para qué: amigos que quedan para jugar; quien aloja abre una sala y reparte permisos, quien
se une entra con un código o una invitación. Lo que leen primero: si hay sala, quién está sentado
en ella y cómo va la conexión. Restricciones: convenciones de GNOME/KDE y de Material 3, tema y
acento del sistema, contraste AA con cualquier acento, textos en español, datos de ejemplo ficticios.

Decidido sin el propietario delante (pidió el rediseño sin poder contestar): la dirección es la
que asignó la tirada, sin retadores porque el catálogo no era accesible desde este entorno.

## Direction contract

THESIS: coroom es una sala: una pantalla, unas butacas y un código en una entrada. Rechaza las dos
salidas de la categoría: el vestíbulo «gamer» oscuro con neón y la ventana gris de toolkit con un
solo color de acento.

OWN-WORLD: las luces de la sala salen del acento del sistema. Los neutros van teñidos de ese tono
(claro: vestíbulo; oscuro: sala a media luz) y el riel lateral lleva el tinte más decidido. Los
estados usan las lámparas de la sala: verde de «salida» (conectado, directo), ámbar de pasillo
(relay, calidad justa), piloto rojo de «en vivo». Las personas son butacas: avatar de respaldo
redondeado arriba y base recta, con su color y una lámpara de estado. Los códigos y las cifras que
la app enseña van en numeración de señalética: grandes, seminegrita, agrupados de tres en tres.
El campo donde se escribe un código es el control de texto de la plataforma y muestra lo que se
teclea, sin celdas.

STORY: quien abre coroom ve dónde sentarse: compartir su pantalla es la acción principal, unirse es
escribir un código en una entrada, y los amigos conectados están a un clic de invitar. Dentro de la
sala ve quién ocupa cada butaca, qué puede hacer y cómo le llega.

FIRST VIEWPORT (Inicio, 1180×780): el marco es el de la app: cabecera de ventana con el título
de la página y la píldora del servidor con su latencia, y un riel teñido que a este ancho va
plegado en iconos (60 px, con la marca arriba y tu butaca con la lámpara del servidor abajo) y se
despliega con nombres desde unos 1300 px, como la barra lateral adaptable de GNOME. En la página,
a 3/5 del ancho, el bloque «Compartir mi pantalla» con el botón principal y la fila de butacas de
tu sala; a 2/5, «Unirse con código» como entrada: campo de texto estándar y el talón «Entrar».
Debajo, la Bandeja con las invitaciones recibidas como entradas ya emitidas (código en
señalética, cuenta atrás, talón «Entrar») y, al pie, los amigos en línea como butacas, a un clic
de invitar cuando hay sala abierta. Sin saludo: el título de la página ya dice dónde estás.

SIGNATURE: «la fila de butacas». Allí donde hay una sala, sus integrantes se ven como una fila de
butacas numeradas: la del anfitrión primero, ocupadas con el color de cada persona y su lámpara de
enlace, libres en contorno. Al entrar alguien, su butaca se enciende; al hablar, su anillo se pone
verde. Las butacas libres numeradas solo aparecen en tu propia sala como anfitrión (ahí
significan «puedes invitar»): el invitado no conoce el aforo y solo ve a quienes están. Movimiento: el de la casa, breve (entradas de 140 a 260 ms con salida más rápida), nunca
decorativo.

FORM: sala de cine (entrada, plano de butacas, señalética); puesto 6 de 7 en la lista propia;
seed:f9d3823554bfe892

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Sin decidir

- El propietario no ha visto la dirección: si prefiere otra, la tirada admite repetirse.
- Android no se pudo dibujar en el entorno donde se hizo (sin SDK ni emulador): se verificó por
  compilación y lint en la CI y con una auditoría del código. Sus colores de butaca y algunas
  proporciones difieren de los del escritorio (ver `DESIGN.md`).
- En la sala del anfitrión, con la ventana estrecha (900 px), el chat de la sala conserva su
  columna aunque esté vacío: plegarlo cambia cuándo se ve el chat y no se tocó en el rediseño.
