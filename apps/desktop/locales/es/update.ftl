# Textos de la versión nueva (identificadores con el prefijo `update-`): los avisos de Inicio, la
# fila «Versión nueva» de Ajustes → Acerca de y el aviso de versiones distintas con el servidor.

## Botones
update-novedades = Novedades
update-reiniciar-y-actualizar = Reiniciar y actualizar
update-cierra-la-sala = Cierra la sala para reiniciar
update-reintentar = Reintentar
update-abrir-la-carpeta = Abrir la carpeta

## Inicio
update-lista-titulo = coroom { $version } está lista para instalar
update-lista-texto = Tienes la { $current }. Si no reinicias ahora, se usa la próxima vez que abras coroom.
update-lista-con-sala = Podrás reiniciar cuando cierres la sala; si no, se usa la próxima vez que abras coroom.
update-no-se-pudo-descargar = No se pudo descargar coroom { $version }
update-no-se-pudo-descargar-texto = { $err }. Se reintenta en la próxima búsqueda.
update-carpeta-titulo = No se puede actualizar en esta carpeta
update-carpeta-texto = Windows no deja escribir en { $folder }. Mueve la carpeta de coroom a una tuya (por ejemplo, Documentos) y se actualizará sola.
# Al arrancar, si la versión nueva no se pudo instalar al reiniciar.
update-no-se-pudo-instalar = No se pudo instalar la versión nueva: { $err }
# La versión que no se pudo instalar: no se vuelve a descargar sola.
update-no-se-pudo-instalar-titulo = No se pudo instalar coroom { $version }
update-no-se-pudo-instalar-texto = { $err }. No se vuelve a descargar sola: pulsa «Reintentar».

## Ajustes → Acerca de
update-descargando = Descargando coroom { $version } · { $percent } % de { $size } MB
update-descargando-sin-tamano = Descargando coroom { $version } · { $percent } %
update-en-pausa = Descarga de coroom { $version } en pausa mientras hay una sala abierta ({ $percent } %).
update-lista-acerca = coroom { $version } está lista. Se usa al reiniciar o la próxima vez que abras coroom.
update-fallo-acerca = No se pudo descargar coroom { $version }: { $err }. Se reintenta en la próxima búsqueda.
update-fallo-instalar-acerca = No se pudo instalar coroom { $version }: { $err }. No se vuelve a descargar sola.
update-msix = Hay una versión nueva: { $version }. Este paquete (MSIX) no puede actualizarse solo: bájala de la página de la versión.
update-automatico = Actualizar automáticamente
update-automatico-ayuda = Descarga e instala las versiones nuevas de GitHub. Nunca descarga con una sala abierta ni reinicia sin preguntarte.

## Versiones distintas con el servidor
update-app-vieja = Tu coroom es más viejo que el servidor
update-app-vieja-texto = { $host } usa el protocolo { $server } y tu app el { $client }. Actualiza para conectarte.
update-actualizar-y-reiniciar = Actualizar y reiniciar
update-descargar-y-actualizar = Descargar y actualizar
update-porcentaje = { $percent } %
update-servidor-viejo = El servidor necesita actualizarse
update-servidor-viejo-texto = { $host } usa el protocolo { $server } y tu app el { $client }. Avísale a quien lo administra que lo actualice; mientras tanto no puedes conectarte a él.
