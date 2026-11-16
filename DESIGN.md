---
name: coroom
description: Una sala para jugar con amigos; las luces salen del acento del sistema, las personas son butacas y los códigos van en señalética.
colors:
  # Acento por defecto (azul de GNOME). Todo lo demás se calcula a partir de él en
  # crates/ui/src/theme.rs; estos son los valores resueltos con ese acento.
  accent-base: "#3584e4"
  accent-bg: "#3986e5"
  accent-bg-hover: "#4486d7"
  on-accent: "#1c1d22"
  accent-text: "#245a9d"
  focus: "#2864ad"
  # Planos en claro: «vestíbulo»
  window: "#f1f4f7"
  sidebar: "#dae2ed"
  card: "#ffffff"
  card-hover: "#f5f9fe"
  dialog: "#ffffff"
  selected: "#c0d3ec"
  highlight: "#e7f0fc"
  button: "#dee2eb"
  button-hover: "#d0d5df"
  button-pressed: "#c2c8d4"
  text: "#20252e"
  text-secondary: "#56595f"
  text-disabled: "#616569"
  placeholder: "#737579"
  border: "#d6dae5"
  border-strong: "#a4adbe"
  outline: "#768191"
  destructive-bg: "#c21f33"
  destructive-bg-hover: "#a81a2b"
  on-destructive: "#ffffff"
  # Lámparas en claro: sólido (punto, borde), texto, fondo suave, texto sobre el fondo suave
  info-solid: "#3584e4"
  info-text: "#245a9d"
  info-soft: "#e3eefb"
  info-on-soft: "#2b6bb9"
  success-solid: "#1e9e5e"
  success-text: "#11673a"
  success-soft: "#e0f1e8"
  success-on-soft: "#13703f"
  warning-solid: "#c18a16"
  warning-text: "#7c5100"
  warning-soft: "#fbf2df"
  warning-on-soft: "#8a5a00"
  error-solid: "#d7263d"
  error-text: "#af182a"
  error-soft: "#f9e1e4"
  error-on-soft: "#b3192b"
  neutral-solid: "#747d8c"
  neutral-text: "#56595f"
  neutral-soft: "#ecedef"
  neutral-on-soft: "#5f636a"
  live-solid: "#f0392b"
  live-text: "#ab2518"
  live-soft: "#fde3e1"
  live-on-soft: "#c22a1c"
  # Planos en oscuro: «sala a media luz»
  window-dark: "#171c24"
  sidebar-dark: "#0d141d"
  card-dark: "#252a31"
  card-hover-dark: "#2e333a"
  dialog-dark: "#2e333a"
  selected-dark: "#172f4d"
  highlight-dark: "#2f435c"
  button-dark: "#353a40"
  button-hover-dark: "#3e4349"
  button-pressed-dark: "#484c52"
  text-dark: "#f1f3f7"
  text-secondary-dark: "#b4b7bc"
  text-disabled-dark: "#9fa3a8"
  placeholder-dark: "#8d9096"
  border-dark: "#33373e"
  border-strong-dark: "#585c61"
  outline-dark: "#7b7e83"
  accent-bg-hover-dark: "#4990e7"
  accent-text-dark: "#7fb1ed"
  focus-dark: "#68a3eb"
  destructive-bg-hover-dark: "#d42a3f"
  # Lámparas en oscuro
  info-solid-dark: "#3584e4"
  info-text-dark: "#7fb1ed"
  info-soft-dark: "#293e58"
  info-on-soft-dark: "#7cafed"
  success-solid-dark: "#2fb872"
  success-text-dark: "#86e3ad"
  success-soft-dark: "#27493f"
  warning-solid-dark: "#d09624"
  warning-text-dark: "#f6cf6b"
  warning-soft-dark: "#4b422e"
  error-solid-dark: "#e5484d"
  error-text-dark: "#ff8f8a"
  error-soft-dark: "#4f3137"
  neutral-solid-dark: "#86898d"
  neutral-text-dark: "#b4b7bc"
  neutral-soft-dark: "#3a3f45"
  live-solid-dark: "#ff4d3d"
  live-text-dark: "#ff9a8c"
  live-soft-dark: "#553234"
  # Sobre el vídeo (igual en los dos modos)
  video-backdrop: "#000000"
  overlay: "#171c24"
  overlay-text: "#f5f5f7"
  overlay-text-secondary: "#b7b8bc"
  scrim: "#0d141d"
  scrim-dark: "#000000"
  # Butacas: ocho tapizados fijos, letra blanca, los mismos en claro y en oscuro
  seat-1: "#3a6fd8"
  seat-2: "#1a7f8c"
  seat-3: "#2e8548"
  seat-4: "#a36600"
  seat-5: "#c4501a"
  seat-6: "#cf3a4a"
  seat-7: "#c04888"
  seat-8: "#8750c8"
  on-seat: "#ffffff"
typography:
  display:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "48px"
    fontWeight: 600
  title:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "26px"
    fontWeight: 600
  heading:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "18px"
    fontWeight: 600
  body:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "14px"
    fontWeight: 400
  body-strong:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "14px"
    fontWeight: 600
  secondary:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "13px"
    fontWeight: 400
  caption:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "12px"
    fontWeight: 400
  badge:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "12px"
    fontWeight: 600
  sign-large:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "30px"
    fontWeight: 600
  sign-small:
    fontFamily: "Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI, Ubuntu, sans-serif"
    fontSize: "15px"
    fontWeight: 600
  mono:
    fontFamily: "Adwaita Mono, Source Code Pro, Noto Sans Mono, JetBrains Mono, Cascadia Mono, Consolas, DejaVu Sans Mono, Liberation Mono, monospace"
    fontWeight: 400
rounded:
  cell: "6px"
  control: "8px"
  card: "12px"
  dialog: "16px"
  pill: "999px"
spacing:
  xxs: "4px"
  xs: "8px"
  s: "12px"
  m: "16px"
  l: "24px"
  xl: "32px"
components:
  button-primary:
    backgroundColor: "{colors.accent-bg}"
    textColor: "{colors.on-accent}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.control}"
    padding: "8px 16px"
    height: "36px"
  button-primary-hover:
    backgroundColor: "{colors.accent-bg-hover}"
  button-normal:
    backgroundColor: "{colors.button}"
    textColor: "{colors.text}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.control}"
    padding: "8px 16px"
    height: "36px"
  button-normal-hover:
    backgroundColor: "{colors.button-hover}"
  button-normal-pressed:
    backgroundColor: "{colors.button-pressed}"
  button-destructive:
    backgroundColor: "{colors.destructive-bg}"
    textColor: "{colors.on-destructive}"
    rounded: "{rounded.control}"
    padding: "8px 16px"
    height: "36px"
  button-destructive-soft:
    backgroundColor: "{colors.error-soft}"
    textColor: "{colors.error-on-soft}"
    rounded: "{rounded.control}"
    padding: "8px 16px"
  button-link:
    textColor: "{colors.accent-text}"
    rounded: "{rounded.control}"
    padding: "8px 16px"
  card:
    backgroundColor: "{colors.card}"
    textColor: "{colors.text}"
    rounded: "{rounded.card}"
    padding: "16px"
  ticket:
    backgroundColor: "{colors.card}"
    textColor: "{colors.text}"
    rounded: "{rounded.card}"
    padding: "12px 16px"
  ticket-accent:
    backgroundColor: "{colors.highlight}"
    textColor: "{colors.text}"
    rounded: "{rounded.card}"
    padding: "12px 16px"
  input:
    backgroundColor: "{colors.card}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "8px 12px"
    height: "36px"
  nav-item-selected:
    backgroundColor: "{colors.selected}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    size: "44px"
  badge-success:
    backgroundColor: "{colors.success-soft}"
    textColor: "{colors.success-on-soft}"
    typography: "{typography.badge}"
    rounded: "{rounded.pill}"
    padding: "3px 10px"
  badge-warning:
    backgroundColor: "{colors.warning-soft}"
    textColor: "{colors.warning-on-soft}"
    typography: "{typography.badge}"
    rounded: "{rounded.pill}"
    padding: "3px 10px"
  badge-live:
    backgroundColor: "{colors.live-soft}"
    textColor: "{colors.live-on-soft}"
    typography: "{typography.badge}"
    rounded: "{rounded.pill}"
    padding: "3px 10px"
  code-cell-large:
    backgroundColor: "{colors.highlight}"
    textColor: "{colors.text}"
    typography: "{typography.sign-large}"
    rounded: "{rounded.control}"
    width: "40px"
    height: "52px"
  code-cell-small:
    backgroundColor: "{colors.highlight}"
    textColor: "{colors.text}"
    typography: "{typography.sign-small}"
    rounded: "{rounded.cell}"
    width: "20px"
    height: "26px"
  seat-row:
    backgroundColor: "{colors.seat-1}"
    textColor: "{colors.on-seat}"
    size: "44px"
  seat-list:
    backgroundColor: "{colors.seat-1}"
    textColor: "{colors.on-seat}"
    size: "36px"
  seat-compact:
    backgroundColor: "{colors.seat-1}"
    textColor: "{colors.on-seat}"
    size: "32px"
  dialog:
    backgroundColor: "{colors.dialog}"
    textColor: "{colors.text}"
    rounded: "{rounded.dialog}"
    padding: "24px"
    width: "440px"
---

# Design System: coroom

Este documento describe el sistema visual tal como está en el código, no como se planeó. La fuente
es `crates/ui/src` (el escritorio: `theme.rs` es el único archivo con colores fijos y lo vigila
`tests/no_fixed_colors.rs`; las medidas están en `tokens.rs`) y
`packaging/android/app/src/main/kotlin/coroom/android/ui` (el invitado Android). El escritorio se
comprobó además con las capturas en claro y en oscuro de `.impeccable/review/`. **Android se
documenta solo desde su código**: no hay capturas suyas.

Los valores del bloque de arriba son los normativos. Los colores son los que resultan con el acento
por defecto (azul); con otro acento cambian, y lo que se mantiene es la receta de la sección
[Colors](#colors).

## Overview

**Creative North Star: "La sala"**

coroom es una sala: una pantalla, unas butacas y un código en una entrada. La ventana toma las
luces de la sala del color de acento del sistema: en claro es el vestíbulo, en oscuro la sala a
media luz, y en ninguno de los dos hay un gris puro. Las personas son butacas, los estados son las
lámparas de la sala y los códigos van en señalética. Todo lo demás es un escritorio de GNOME o de
KDE bien hecho: tarjetas con filete fino, listas agrupadas, un riel lateral adaptable, la letra del
sistema.

La densidad es la de una aplicación de trabajo: controles de 36 px, cuerpo de 14 px, espaciado en
múltiplos de 4. La interfaz no decora: el color dice estado o identidad, el movimiento dice que
algo cambió, y durante la partida todo lo que flota sobre el vídeo se aparta solo.

El sistema descarta las dos salidas habituales de la categoría: el vestíbulo «gamer» oscuro con
neón y la ventana gris de toolkit con un solo color de acento.

**Key Characteristics:**

- Los neutros llevan el tinte del acento del sistema; el riel lateral es el plano más teñido.
- Todo texto cumple contraste AA (4,5:1) con cualquier acento; lo que se dibuja solo con una línea,
  3:1. El tema ajusta los colores hasta llegar; lo recorren los tests de `crates/ui`.
- Las personas se dibujan siempre con la silueta de la butaca, nunca con un círculo.
- Tres lámparas con un solo significado cada una: verde (bien, directo), ámbar (relay, aviso),
  piloto rojo (se está compartiendo).
- Los códigos de sala van en celdas de ancho fijo, en seminegrita, de tres en tres.
- Las tarjetas no llevan sombra: la sombra es de lo que flota.
- Movimiento breve (de 60 a 420 ms), con la salida al 70 % de la entrada, y nada espera a una
  animación para aceptar un clic.

## Colors

Una paleta calculada: un acento, neutros teñidos de él, tres lámparas y ocho tapizados de butaca.

### Primary

- **Acento del sistema** (`accent-base`): llega del portal `Settings` en Linux o del registro en
  Windows, con cualquier valor; por defecto, el azul de GNOME. Nunca se usa tal cual.
- **Acento como fondo** (`accent-bg`, `accent-bg-hover`, `on-accent`): el botón principal, el
  interruptor encendido, la casilla marcada, el contador de no leídos. La letra de encima es blanca
  o casi negra, la que más contraste dé; con el azul por defecto sale casi negra.
- **Acento como texto** (`accent-text`): enlaces, botones planos destacados, el icono del destino
  elegido en el riel.
- **Foco** (`focus`): el borde de 2 px del campo con el foco, el filete del segmento elegido, el
  borde de la opción elegida.
- **Rellenos suaves de acento** (`selected` sobre el riel, `highlight` sobre tarjetas y diálogos):
  el destino elegido, la opción bajo el ratón de un desplegable, las celdas de señalética, la
  entrada destacada.

### Secondary

Las lámparas de la sala. Cada tono tiene cuatro colores: sólido (el punto, un borde), texto sobre
los planos normales, fondo suave (avisos e insignias) y texto sobre ese fondo.

- **Verde de «salida»** (`success-*`): conectado, enlace directo, paso completado, el anillo de
  quien habla.
- **Ámbar de pasillo** (`warning-*`): relay, calidad justa, conectando, avisos.
- **Piloto rojo de «en vivo»** (`live-*`): solo dice que algo está saliendo de este equipo. Va en
  un punto con su texto, en la etiqueta con el tiempo transmitiendo y en el borde de la tarjeta de
  la transmisión. No es un aviso ni un error.
- **Rojo de error** (`error-*`, `destructive-bg`): validación, fallos y acciones destructivas. Es
  un rojo distinto del piloto (lo comprueba un test).
- **Información** (`info-*`): el acento como tono, para avisos informativos.
- **Neutro** (`neutral-*`): desconectado, sala cerrada.

### Tertiary

- **Tapizados de butaca** (`seat-1` a `seat-8`): ocho colores de la misma luminancia, todos con la
  letra blanca en AA. A cada persona le toca uno estable por su nombre (FNV-1a sobre el nombre), el
  mismo en toda la aplicación y en los dos modos. Solo son identidad: no dicen estado ni permiso.

### Neutral

- **Ventana** (`window`): el fondo de las páginas.
- **Riel** (`sidebar`): el plano con más acento. En claro es más oscuro que el contenido; en
  oscuro es el pasillo, lo más oscuro de la ventana.
- **Tarjeta** (`card`) y **diálogo** (`dialog`): el plano más claro. En oscuro cada plano que se
  acerca es la ventana con más luz: tarjeta, luego diálogo, luego botón.
- **Texto** (`text`, `text-secondary`, `text-disabled`, `placeholder`): cuatro niveles. El
  desactivado también cumple AA: se tiene que poder leer por qué algo está desactivado.
- **Líneas** (`border`, `border-strong`, `outline`): el filete de las tarjetas, el contorno de una
  butaca libre o de un campo bajo el ratón, y lo que se dibuja sin más apoyo (el carril de un
  deslizador, un interruptor apagado, el punteado de una entrada).
- **Sobre el vídeo** (`overlay` al 86 %, `overlay-text`, `overlay-text-secondary`,
  `video-backdrop`): el oscuro de la sala, igual en claro y en oscuro.

### La receta

Con el acento `A` (mezclas lineales en sRGB; «tinta» es `A` llevado a 3:1 contra el blanco en
claro, o contra `#151518` en oscuro):

| Plano | Claro | Oscuro |
|---|---|---|
| Ventana | `#f7f7f8` + 3 % de tinta | `#151518` + 6 % de tinta |
| Riel | `#ececee` + 10 % | `#0a0a0c` + 8 % |
| Tarjeta | blanco | ventana + 6 % de blanco |
| Diálogo | blanco | ventana + 10 % de blanco |
| Botón normal | `#e7e7eb` + 5 % | ventana + 13 % de blanco (17 % bajo el ratón, 21 % pulsado) |
| Texto | `#1f2126` + 4 % | `#f5f5f7` + 2 % |
| Texto secundario | texto + 30 % de ventana | texto + 28 % de ventana |
| Filete | `#e0e0e5` + 6 % | ventana + 12 % de blanco |
| Contorno marcado | `#b0b2ba` + 10 % | ventana + 28 % de blanco |
| Elegido en el riel | riel + 16 % de tinta | riel + 24 % |
| Relleno suave | tarjeta + 12 % de tinta | diálogo + 20 % |
| Fondo suave de un tono | sólido + 86 % de tarjeta | sólido + 78 % de tarjeta |
| Velo bajo el ratón / pulsado | texto al 7 % / 12 % | texto al 8 % / 14 % |
| Velo de un diálogo | riel de noche al 36 % | negro al 55 % |

Después de mezclar, cada color de texto se lleva hacia el negro o el blanco en pasos del 2 % hasta
tener 4,5:1 con todos los planos donde puede ir; los sólidos de las lámparas y `outline`, hasta
3:1. Los rellenos suaves se rebajan si el texto no se leyera encima.

### Android

En Android 12 o posterior el esquema es el color dinámico del sistema (Material 3), así que no hay
valores fijos que documentar. En los anteriores hay un esquema propio de respaldo con el azul del
icono (`#0F5CB7` en claro, `#A2C9FE` en oscuro) y todos los neutros teñidos de ese tono. Lo que
Material no tiene como rol va aparte, con valores fijos por modo: las lámparas (punto verde
`#037E38` / `#5ED27D`, punto ámbar `#9C6F02` / `#F1B74A`, piloto `#DA2A32` / `#FF766F`, cada una
con su contenedor), los fondos sobre el vídeo (`#0F141A` al 80 % y al 86 %) y los colores de
butaca. El plano de la navegación es el más teñido: el primario al 8 % sobre `surfaceContainer` en
claro, al 7 % sobre `surfaceContainerLowest` en oscuro. La sala va siempre en oscuro.

### Named Rules

**La regla de las luces.** Ningún neutro es gris puro: todos salen de mezclar un neutro con el
acento del sistema. Un color nuevo se añade a la paleta de `theme.rs` con su receta, no a una
pantalla.

**La regla del piloto.** El rojo de «en vivo» solo dice que se está compartiendo. Nunca va en un
aviso en línea ni en un error, y el rojo de error nunca dice «en vivo».

**La regla de una lámpara, un significado.** Verde es conectado o directo; ámbar es relay o calidad
justa. El estado siempre va también dicho en palabras al lado («Directa», «Relay», «14 ms»).

**La regla del AA con cualquier acento.** Ningún color de texto se fija a mano contra un fondo: se
pide a la paleta, que lo ajusta. Vale igual para un acento amarillo que para uno pizarra.

## Typography

**Display Font:** la del escritorio (la clave `font-name` de GNOME por el portal); si no, la
primera instalada de Adwaita Sans, Cantarell, Inter, Noto Sans, Segoe UI Variable Text, Segoe UI y
Ubuntu; si no, Inter embebida (Regular y SemiBold, subconjunto latino).
**Body Font:** la misma.
**Label/Mono Font:** la primera instalada de Adwaita Mono, Source Code Pro, Noto Sans Mono,
JetBrains Mono, Cascadia Mono, Consolas, DejaVu Sans Mono y Liberation Mono. Solo para datos
técnicos: huellas, números de seguridad, direcciones, diagnóstico.

**Character:** una sola familia en dos pesos, 400 y 600. La jerarquía sale del tamaño y del peso,
no de cambiar de letra; lo propio de coroom es la señalética, que es esa misma letra en seminegrita
dentro de celdas.

### Hierarchy

- **Display** (600, 48 px): una cifra destacada, como el RTT de la estimación de calidad al
  entrar en una sala.
- **Title** (600, 26 px): el título de la página. Uno por pantalla.
- **Heading** (600, 18 px): encabezado de sección o de bloque («Compartir mi pantalla»).
- **Body** (400, 14 px) y **Body strong** (600, 14 px): texto corriente; en seminegrita, títulos
  de fila y de tarjeta y el texto de todos los botones.
- **Secondary** (400, 13 px, color secundario): subtítulos y ayudas.
- **Caption** (400, 12 px, color secundario): pies, el nombre bajo una butaca, etiquetas de tecla.
- **Badge** (600, 12 px): el texto de las insignias de estado.
- **Señalética** (600, 30 px en celda grande, 15 px en celda pequeña): los códigos de sala.

En Android la escala es la de Material 3 tal cual (Roboto del sistema, en `sp`, que sigue el tamaño
de letra del sistema), con los titulares, `titleLarge` y `titleSmall` en seminegrita. La señalética
es `headlineSmall` (o `titleMedium` en seminegrita dentro de una fila) con cifras tabulares, y las
píldoras y las cifras llevan `tnum`.

### Named Rules

**La regla de la señalética.** Un código de sala nunca se escribe de corrido ni en monoespaciada:
cada carácter va en su celda de ancho fijo, en mayúsculas, en seminegrita, agrupados de tres en
tres con un hueco mayor entre grupos. El ancho no cambia con las letras que lleve.

**La regla de los dos pesos.** Solo 400 y 600. Ni cursivas ni versalitas ni un tercer peso.

**La regla de lo que no baila.** Lo que cambia de valor (una latencia, una cuenta atrás) va en una
ranura del ancho de su variante mayor: la píldora no cambia de ancho al cambiar la cifra.

## Layout

El marco es el de una aplicación de GNOME: un riel de navegación a la izquierda, una cabecera con
el título de la página y, a la derecha, la píldora del servidor con su latencia; debajo, la página
con desplazamiento.

- **Riel adaptable.** Plegado mide 60 px y enseña solo iconos (el nombre sale al pasar el ratón);
  desplegado mide 200 px, con nombres. Se despliega desde 1312 px lógicos de ancho de ventana.
  Arriba va la marca; abajo, los ajustes y tu butaca. Cada destino es una zona de 44 px. El ancho
  es fijo en cada modo: lo que cambie en el riel no mueve la página.
- **Página.** Contenido centrado con un ancho máximo de 880 px y un margen de 24 px arriba y abajo
  y 32 px a los lados. Las tarjetas de inicio de sesión y los diálogos miden 440 px.
- **Espaciado.** Escala de 4, 8, 12, 16, 24 y 32 px. Lo habitual: 8 entre un icono y su texto, 12
  entre los elementos de una fila, 16 de relleno de una tarjeta, 24 de relleno de un diálogo.
- **Controles.** 36 px de alto (botones, campos); iconos de 16 px en botones y filas y de 32 px en
  estados vacíos; filas de lista con 10 px arriba y abajo y 16 a los lados.
- **Filas iguales.** Las tarjetas de una misma fila miden lo que la más alta (la entrada de «Unirse
  con código» es tan alta como el bloque de compartir y el punteado la recorre entera).
- **Nada salta.** Lo que puede aparecer o desaparecer (una lámpara, un anillo, un icono de estado)
  tiene su hueco reservado: encenderlo no mueve lo de alrededor.
- **Sobre el vídeo.** Barra superior de 52 px que se oculta a los 2,5 s sin mover el ratón; panel
  de estadísticas de 340 px.
- **Android.** Barra de navegación inferior o riel según el ancho, con los objetos táctiles de
  48 dp de Material. La fila de butacas se desliza en horizontal si no cabe.

## Elevation & Depth

La profundidad se dice con planos, no con sombras. En claro, la ventana teñida queda detrás de las
tarjetas blancas; en oscuro, cada plano que se acerca lleva más luz. Las tarjetas, las entradas y
las filas llevan un filete de 1 px y ninguna sombra.

### Shadow Vocabulary

Todas caen hacia abajo y van difuminadas. El color es el del riel de noche en claro y negro en
oscuro; las opacidades son claro / oscuro.

- **Diálogo** (`0 8px 28px`, 22 % / 55 %): diálogos modales.
- **Toast** (`0 4px 16px`, 25 % / 50 %): avisos breves.
- **Menú** (`0 4px 16px`, 15 % / 45 %): desplegables.
- **Botón bajo el ratón** (`0 2px 6px`, 14 % / 40 %): los botones con relleno suben 1 px.
- **Botón principal bajo el ratón** (`0 2px 10px` del acento, 35 % / 45 %).
- **Brillo de «en vivo»** (`0 2px 18px` del piloto, hasta 45 %): una sola vez al empezar a
  compartir; en reposo, nada.

### Named Rules

**La regla de lo que flota.** Solo lleva sombra lo que está encima de otra cosa (diálogo, menú,
toast) o lo que responde al ratón. Una tarjeta en reposo, nunca.

## Shapes

Esquinas redondeadas en una escala corta y una silueta propia.

- **Radios.** 6 px (celda pequeña de señalética, etiqueta de tecla), 8 px (controles: botones,
  campos, menús, celda grande de señalética), 12 px (tarjetas, entradas, avisos, toasts), 16 px
  (diálogos) y píldora (insignias, puntos, interruptores, la píldora de latencia).
- **La butaca.** Un cuadrado con el respaldo redondeado arriba (radio del 42 % del lado) y la base
  casi recta (18 %). Todo va en proporción al lado, así que se reconoce igual a 20 px que a 72: las
  iniciales miden el 40 % del lado; el número de una butaca libre, el 36 %. La lámpara de estado va
  en la esquina inferior derecha: diámetro del 28 % del lado con un mínimo de 8 px, separada de la
  butaca por un filete de 2 px del color del plano de detrás. La butaca libre es el mismo contorno
  sin relleno, con una línea de 1,5 px. El anillo de quien habla repite la silueta a 3 px, con 2 px
  de grosor en verde. Lados en uso: 44 px en una fila de sala, 36 px en una lista, 32 px sobre el
  vídeo; con el nombre debajo, cada butaca ocupa una ranura de 68 px.
- **El punteado.** La línea que separa el cuerpo de una entrada de su talón: puntos de 2 px cada
  6 px, enteros y centrados en el alto, en el color `outline`.
- **Líneas.** Filete de 1 px en tarjetas; 1,5 px en la entrada destacada y en la tarjeta «en
  vivo»; 2 px para el foco, la opción elegida y un campo con error.

En Android los radios son los de Material con otros valores (6, 10, 14, 20 y 28 dp) y la butaca
tiene las mismas proporciones (42 % y 18 %).

### Named Rules

**La regla de la butaca.** Una persona es siempre una butaca: su foto recortada a la silueta o sus
iniciales sobre su color. No hay avatares redondos.

## Components

### Buttons

Compactos y directos: 36 px de alto, texto de 14 px en seminegrita, radio de 8 px, relleno de 8 por
16 px, con un icono de 16 px delante si lo lleva.

- **Principal:** fondo de acento. Uno por bloque: la acción que el bloque existe para hacer
  («Compartir», «Entrar»).
- **Normal:** el neutro teñido (`button`); se oscurece bajo el ratón y más al pulsar.
- **Plano y enlace:** sin fondo; bajo el ratón, el velo translúcido. El enlace lleva el texto en
  acento.
- **Destructivo:** lleno (rojo, letra blanca) solo para cerrar la sala y salir; suave (fondo rojo
  tenue, texto rojo) para dejar de compartir, expulsar u olvidar un servidor; plano (solo texto
  rojo) dentro de menús y filas.
- **Estados:** los que tienen relleno suben 1 px con sombra bajo el ratón y se hunden al 96 % al
  pulsar (60 ms, vuelven con resorte). Desactivado: fondo `button` y texto `text-disabled`.
- **Sobre el vídeo:** transparentes con texto claro; bajo el ratón, texto claro al 18 %; activos,
  fondo de acento.

### Chips

- **Insignia de estado:** píldora con el fondo suave del tono, un punto de 8 px del color sólido y
  texto de 12 px en seminegrita; relleno de 3 por 10 px. Sobre el vídeo, el sólido al 22 % con
  texto claro. La variante con cifra no cambia de ancho.
- **Etiqueta «en vivo»:** píldora con el piloto lleno y letra blanca, con el tiempo transmitiendo.
- **Selector segmentado:** grupo con fondo `button` y radio de 8 px; el elegido lleva el relleno
  suave de acento y un filete de acento de 1 px, y se desliza hasta el nuevo. El texto del elegido
  va en seminegrita, pero cada segmento mide siempre lo que su texto en seminegrita.

### Cards / Containers

- **Tarjeta:** fondo `card`, filete de 1 px, radio de 12 px, relleno de 16 px, sin sombra.
- **Tarjeta pulsable:** igual; bajo el ratón pasa a `card-hover` con el contorno marcado.
- **Opción excluyente:** tarjeta con borde de 2 px; la elegida, borde de `focus` y relleno suave.
- **Tarjeta «en vivo»:** el borde pasa al rojo del piloto (1,5 px) mientras se comparte.
- **Lista agrupada:** filas dentro de una tarjeta con separadores (la `boxed-list` de GNOME).
- **Aviso en línea:** fondo suave del tono, texto sobre ese fondo, borde del sólido al 35 %, radio
  de 12 px. Tonos: información, bien, aviso, error. Nunca «en vivo».
- **Diálogo:** 440 px, fondo `dialog`, radio de 16 px, relleno de 24 px, sobre un velo. Entra
  subiendo en 200 ms.
- **Toast:** el oscuro de la sala al 96 % con texto claro, radio de 12 px, abajo a la derecha;
  dura 4 s.

### Inputs / Fields

- **Campo:** fondo `card`, filete de 1 px, radio de 8 px, 36 px de alto. Bajo el ratón, contorno
  marcado.
- **Foco:** borde de 2 px en `focus`. La selección es el acento al 35 %.
- **Error:** borde de 2 px en el rojo de error y el motivo debajo, en español y con la causa.
- **Desactivado:** fondo `button` y texto `text-disabled`.
- **Interruptor:** apagado, carril lleno en `outline`; encendido, acento. Desactivado no es
  «apagado»: el carril se vacía y queda su contorno de 1,5 px.
- **Deslizador:** carril de 4 px, tirador redondo de 9 px de radio, valor escrito a la derecha.
- **Casilla:** radio de 6 px; marcada, fondo de acento.

### Navigation

El riel va sobre el plano `sidebar`. Cada destino es un icono de 20 px en una zona de 44 px (con su
nombre al lado cuando el riel está desplegado). El elegido lleva el relleno `selected` con radio de
8 px, sin franja en el borde, y el icono en acento; su fondo se desliza con un resorte hasta el
nuevo. Los demás, el velo bajo el ratón. Los contadores van en una marca pequeña en la esquina del
icono. La cabecera de la ventana lleva el fondo de las tarjetas, el título de la página y la
píldora del servidor.

### La fila de butacas

La firma del sistema. Donde hay una sala, sus integrantes se ven como una fila de butacas: la del
anfitrión primero, las ocupadas con el color o la foto de cada persona y la lámpara de su enlace
(verde: directo; ámbar: relay), y el anillo verde mientras alguien habla. Si no caben todas, la
última dice cuántas faltan («+3»).

Las butacas libres, en contorno y numeradas, solo aparecen en tu propia sala como anfitrión, hasta
completar cuatro: ahí significan «puedes invitar». El invitado no conoce el aforo y solo ve a
quienes están; mientras entra, su sitio es una butaca en contorno sin número. El nombre va debajo
(una línea, con «…» si no cabe), en un texto emergente o en ningún sitio si ya está en una lista al
lado. Al entrar alguien su butaca se enciende; la lámpara cambia de tono con un fundido.

En Android la fila es la misma idea con otras medidas: butacas de 44 dp con 8 dp entre ellas, sin
butacas libres (nunca se conoce el aforo), una sola inicial en vez de dos, lámpara del 30 % del
lado con un mínimo de 12 dp y un fundido de 220 ms al ocuparse.

### La entrada

Una tarjeta con el cuerpo a la izquierda (quién invita y a qué sala, o el campo para escribir un
código), el punteado vertical y el talón a la derecha con la acción («Entrar», «Copiar»). Es una
tarjeta como las demás: el punteado es lo único que la distingue. El cuerpo y el talón llevan un
relleno de 12 por 16 px. La variante destacada, con relleno suave y borde de acento de 1,5 px, es
para la que pide respuesta ya (alguien que pide entrar): como mucho una a la vez en pantalla.

En Android la entrada va rellena con `primaryContainer`, con radio de 20 dp, el punteado en el
color del contenido al 50 % y el talón entero pulsable (80 dp de ancho como poco).

### El código en señalética

Cada carácter en su celda con el relleno suave de acento, sin borde. Celda grande para el código de
la sala abierta: 40 por 52 px, letra de 30 px, 4 px entre celdas y 16 entre grupos. Celda pequeña
dentro de una invitación: la mitad en todo (20 por 26 px, letra de 15 px, 2 y 8 px). En Android el
código va sin fondo de celda: cada carácter en una ranura de 0,9 veces el tamaño de la letra y
media ranura entre grupos.

### Movimiento

| Token | Duración | Curva | Para qué |
|---|---|---|---|
| Hundirse | 60 ms | `ease-out-expo` | el botón al pulsarlo |
| Pasar el ratón | 90 ms | lineal | color y fondo |
| Rápido | 140 ms | `ease-out-expo` | interruptores, insignias |
| Panel | 200 ms | `ease-out-expo` | paneles, desplegables, toasts, diálogos |
| Pantalla | 260 ms | `ease-out-expo` | cambio de pantalla; la nueva se desliza 36 px |
| Momento | 420 ms | `ease-out-expo` | abrir la sala, entrar en una, pasarla |

Todo lo que sale dura el 70 % de su entrada, con `ease-in-out`. Los bloques de una pantalla y los
elementos de una lista se escalonan 30 ms; las opciones de un desplegable, 15 ms. Los interruptores,
los toasts y el fondo del destino elegido se mueven con un resorte críticamente amortiguado. Con
movimiento reducido quedan fundidos cortos y se quitan desplazamientos, escalas y escalonados; el
factor de duración de KDE multiplica las duraciones y con 0 no se anima nada.

## Do's and Don'ts

### Do:

- **Do** pedir todos los colores a la paleta de `crates/ui/src/theme.rs`; es el único sitio con
  colores fijos y un test lo comprueba.
- **Do** dibujar a cada persona como una butaca, con los radios del 42 % y del 18 % del lado.
- **Do** decir el estado de la conexión con su lámpara y con palabras («Directa», «Relay · 61 ms»).
- **Do** poner los códigos de sala en celdas de señalética, de tres en tres.
- **Do** usar una entrada, con su punteado y su talón, para lo que da paso a una sala: una
  invitación, unirse con un código, el código de la sala propia.
- **Do** dejar un solo botón principal por bloque.
- **Do** reservar el hueco de lo que aparece y desaparece, para que nada se mueva.
- **Do** respetar el movimiento reducido y el factor de duración del sistema.
- **Do** seguir en cada plataforma sus convenciones: en el escritorio las de GNOME y KDE, en
  Android Material 3 con el color dinámico del sistema.

### Don't:

- **Don't** usar un gris puro ni un color fijo fuera de `theme.rs`.
- **Don't** usar el rojo de «en vivo» para un aviso o un error, ni el de error para decir que se
  está compartiendo.
- **Don't** poner sombra a una tarjeta en reposo.
- **Don't** dibujar avatares redondos.
- **Don't** enseñar butacas libres numeradas al invitado: no conoce el aforo.
- **Don't** escribir un código de sala de corrido o en monoespaciada.
- **Don't** añadir un tercer peso de letra ni otra familia.
- **Don't** hacer que algo espere a una animación para aceptar un clic.
- **Don't** destacar más de una entrada a la vez.
- **Don't** hacer el vestíbulo «gamer» oscuro con neón ni la ventana gris de toolkit con un solo
  color de acento.
