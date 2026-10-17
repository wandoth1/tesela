# Textos de la interfaz en español. Todo lo que ve el usuario sale de aquí: no hay cadenas de interfaz en el código.
# Hay un test que exige que `es.ftl` y `en.ftl` tengan exactamente los mismos identificadores y que el código solo use
# identificadores que existen.

app-name = Tesela
settings = Ajustes
settings-theme = Tema
settings-language = Idioma
about = Acerca de Tesela
about-version = Versión { $v }
about-description = Analizador de espacio en disco para Windows. Te enseña qué ocupa espacio y te ayuda a liberarlo: lo regenerable se borra, tus archivos van a la Papelera y las carpetas del sistema están protegidas.
about-license = Licencia: MIT o Apache-2.0
about-notices = Avisos de terceros
theme-system = Igual que el sistema
theme-light = Claro
theme-dark = Oscuro
lang-system = Igual que el sistema
lang-es = Español
lang-en = English
cancel = Cancelar
analyze = Analizar
analyze-drive = Analizar { $name }
analyze-folder = Analizar una carpeta…
rescan = Reescanear (F5)
search = Buscar
search-hint = Buscar…  Ctrl+F
up-level = Subir un nivel
go-home = Inicio
go-to-folder = Ir a { $name }
next-version = Disponible en la próxima versión
next-version-v02 = Disponible en la versión 0.2
demo-banner = Modo demostración: datos de ejemplo, nada se lee de tu disco
demo-unavailable = No disponible en el modo demostración
title-scanning = Analizando { $name }

# Inicio
home-title = ¿Qué disco quieres revisar?
home-subtitle = Te enseñamos qué ocupa espacio en tu disco.
home-privacy = Todo se analiza en tu equipo. Sin cuentas, sin anuncios, sin enviar datos.
home-loading-drives = Buscando unidades…
home-no-drives = No se ha encontrado ninguna unidad. Puedes analizar una carpeta.
drive-used = { $pct } % usado
drive-used-word = usado
drive-free-of = { $free } libres de { $total }
drive-quota = Disponible para tu cuenta (cuota): { $avail }
drive-quota-scope = Cifras de tu cuenta (cuota de disco)
drive-almost-full = Casi lleno
drive-local-disk = Disco local
drive-removable-disk = Disco extraíble
drive-network-disk = Unidad de red
drive-removable-note = Extraíble · sin Papelera
drive-network-note = Red · sin Papelera
error-scan-start = No se pudo empezar el análisis: { $error }
error-scan-root-missing = la ruta no existe o no está disponible
error-scan-root-denied = no tienes permiso para abrir esa ruta
error-scan-root = no se puede abrir la ruta ({ $detail })
error-scan-not-dir = la ruta no es una carpeta ni una unidad
error-scan-root-link = la carpeta elegida es un enlace (junction o symlink); Tesela no sigue enlaces. Elige la carpeta de destino
error-scan-panicked = un hilo del análisis falló de forma inesperada
error-scan-startup-lost = el arranque del análisis se interrumpió
error-scan-cancelled = el análisis se canceló
error-open-explorer = No se pudo abrir el Explorador: { $error }
error-reveal-invalid = la ruta no es válida
error-reveal-parse = Windows no reconoce la ruta ({ $detail })
error-reveal-open = el Explorador no pudo mostrarla ({ $detail })
error-reveal-timeout = El Explorador no responde: puede tardar en abrirse
error-reveal-busy = El Explorador sigue ocupado con una petición anterior

# Escaneo
scan-title = Mientras analizamos tu disco…
scan-counts = { $files } archivos · { $size } · { $time }
scan-counts-of = { $files } archivos · { $size } de { $total } · { $time }
scan-stop = Parar y ver lo que hay
scan-progress-label = Progreso del análisis: { $pct } %
scan-known-title = Sitios donde suele haber archivos temporales
scan-known-sum = Hasta { $size } en candidatos de bajo riesgo (se comprueba al limpiar)
scan-known-note = Es una estimación, no un mínimo garantizado, y solo cuenta lo de bajo riesgo: temporales de más de 7 días y cachés de navegador. Qué se borra y qué va a la Papelera te lo enseñamos antes de limpiar, y la cifra real se mide al limpiar.
scan-known-excluded = Descargas y la Papelera son datos tuyos: se muestran, pero no cuentan en la cifra de arriba.
scan-known-excluded-diag = Los informes de errores y los volcados de bloqueo sirven para diagnosticar fallos: tampoco cuentan en la cifra de arriba.
risk-review-tag = Revisar
scan-clean-safe = Limpiar lo seguro
scan-checking = Revisando { $done } de { $total } sitios conocidos…
scan-checked = Sitios conocidos revisados
scan-searching = buscando…
scan-no-known = No hay sitios conocidos dentro de esta carpeta.
scan-errors =
    { $n ->
        [one] { $n } carpeta sin permiso hasta ahora
       *[other] { $n } carpetas sin permiso hasta ahora
    }
scan-map-title = El mapa se va llenando
scan-counting = ≥ = aún contando
scan-root-files = Archivos sueltos
scan-fast-mode = ¿Va lento? Con permisos de administrador el análisis tarda segundos. Usar el modo rápido
known-user-temp = Temporales del usuario
known-windows-temp = Temporales de Windows
known-chrome-cache = Caché de Chrome
known-edge-cache = Caché de Edge
known-firefox-cache = Caché de Firefox
known-downloads = Descargas
known-recycle-bin = Papelera de reciclaje
known-wer-reports = Informes de errores de Windows
known-crash-dumps = Volcados de bloqueo de programas
known-other = Otro sitio conocido

# Ventana principal
sidebar-drives = Unidades
sidebar-views = Vistas
sidebar-types = Leyenda
view-map = Mapa
view-list = Lista
view-recommendations = Recomendaciones
legend-mixed = Mixto
legend-mixed-tip = Carpeta sin tipo dominante: ningún tipo supera el 50 % de sus bytes. La franja de abajo muestra los tres principales.
legend-info-only = Solo informar
legend-info-only-tip = Archivos del sistema que solo se informan: no se pueden borrar desde aquí.
legend-system = Sistema
legend-system-tip = Sistema / sin medir: espacio ocupado que el análisis no pudo ver.
color-by = Colorear:
color-type = Tipo
color-age = Antigüedad
color-risk = Riesgo
type-video = Vídeo
type-image = Imágenes
type-audio = Audio
type-document = Documentos
type-archive = Comprimidos
type-program = Programas
type-other = Otros
type-share = { $pct } { $type }
others-title = Otros ({ $n } elementos, { $size })
system-block = Windows y otros datos del sistema
system-tip = Incluye archivos de Windows, hibernación, puntos de restauración y carpetas que tu usuario no puede ver. Es normal y no se puede borrar desde aquí.
system-admin-link = Ver el detalle (modo administrador)
info-pagefile = Archivo de paginación de Windows. Windows lo gestiona; no se puede borrar desde aquí.
info-hiberfil = Archivo de hibernación de Windows. Si no usas la hibernación se puede reducir o desactivar desde un símbolo del sistema ejecutado como administrador: powercfg /h /type reduced lo encoge y conserva el inicio rápido; powercfg /h off lo elimina, pero también quita la suspensión híbrida y el inicio rápido. Tesela no ejecuta nada.
info-swapfile = Archivo de intercambio de las aplicaciones de Windows. Windows lo gestiona.
risk-info-only = Solo informar
incomplete-note = El análisis se detuvo antes de recorrer todo: este tamaño es un mínimo.
cloud-note = Archivos de la nube (OneDrive u otro): los que solo están en la nube no ocupan espacio en este disco.
approx-badge = Aproximado
approx-tip = Este análisis usó el modo de respaldo de Windows (entero o en algunas carpetas): los tamaños en disco son estimados y los enlaces duros pueden contarse más de una vez.
tip-enter = Doble clic para entrar · clic derecho para más
tip-open-group = Doble clic para abrir la agrupación y ver sus elementos
tip-pct-of = { $pct } de { $parent }
files-count =
    { $n ->
        [one] { $count } archivo
       *[other] { $count } archivos
    }
share-of-folder = { $pct } de la carpeta
map-label = Mapa de { $name }: { $n } elementos.
map-selected = Seleccionado: { $name }, { $size }.
details-selection = Selección
details-folder = Carpeta actual
details-on-disk = En disco
details-size = Tamaño
details-files = Archivos
details-of = De { $name }
details-of-disk = Del disco
details-biggest = Lo más grande aquí
menu-enter = Entrar
menu-open-group = Abrir
menu-open-explorer = Abrir en el Explorador
menu-copy-path = Copiar ruta
menu-properties = Propiedades
menu-add-basket = Añadir a la cesta
menu-unavailable-system = No disponible para este elemento
path-copied = Ruta copiada
list-col-name = Nombre
list-col-share = Porcentaje
list-col-size = En disco
list-col-files = Archivos
list-empty = Esta carpeta está vacía.
list-no-results = Ningún elemento coincide con la búsqueda.
basket-summary =
    { $n ->
        [one] Cesta · { $n } elemento · { $size }
       *[other] Cesta · { $n } elementos · { $size }
    }
basket-view = Ver
basket-empty = Vaciar cesta
basket-clean = Limpiar { $size }
status-volume = { $used } usados de { $total } · { $files } archivos · analizado { $ago }
status-folder = { $size } en esta carpeta · { $files } archivos · analizado { $ago }
status-stopped = Análisis detenido: las cifras con ≥ son un mínimo.
status-gap = El mapa explica { $explained } de { $used }. ¿Y el resto?
status-no-access =
    { $n ->
        [one] { $n } carpeta sin permiso
       *[other] { $n } carpetas sin permiso
    }
ago-now = hace menos de 1 min
ago-minutes = hace { $n } min
ago-hours = hace { $n } h
age-over-month = El { $pct } no se modifica desde hace más de 1 mes
age-over-6months = El { $pct } no se modifica desde hace más de 6 meses
age-over-year = El { $pct } no se modifica desde hace más de 1 año
age-over-3years = El { $pct } no se modifica desde hace más de 3 años
age-recent = El { $pct } se modificó en el último mes
age-mixed = Mezcla de archivos recientes y antiguos
map-has-others = Incluye la agrupación { $label }.
percent = { $n } %
percent-fine = { $v } %
scan-measuring = Calculando lo que se puede liberar…
scan-nothing-safe = No hay nada seguro que limpiar en los sitios conocidos
scan-clean-wait = Se activa cuando acaba el barrido de los sitios conocidos
scan-row-blocked = Cierra { $name } para liberar hasta { $size }

# Cesta, protección y menú
basket-added = Añadido a la cesta
basket-empty-hint = La cesta está vacía. Añade elementos desde el mapa, la lista o Recomendaciones.
basket-window-title = Cesta
basket-contents-of = Contenido de { $name }
basket-remove-item = Quitar { $name } de la cesta
basket-phrase-permanent = Se elimina y se vuelve a crear sola
basket-phrase-recycle = Va a la Papelera de reciclaje · puedes recuperarlo
basket-phrase-bin = Se elimina definitivamente lo que hay en la Papelera al confirmar
menu-add-contents = Añadir el contenido de «{ $name }» a la cesta
menu-add-contents-short = Añadir su contenido a la cesta
menu-remove-basket = Quitar de la cesta
menu-in-basket = Ya está en la cesta
menu-in-basket-why = Está dentro de algo que ya está en la cesta.
menu-view-contents = Ver contenido
menu-unavailable-cloud = Los archivos que solo están en la nube no ocupan espacio en este disco: no se pueden seleccionar para liberar espacio.
menu-unavailable-info = Solo informar: Windows lo gestiona y no se puede borrar desde aquí.
protected-label = Protegida
protected-denied-why = Está protegida: es del sistema o de otros programas y no se puede borrar desde aquí.
protected-untouchable-why = Es una carpeta clave de tu perfil o de tu disco: no se puede borrar entera, pero sí limpiar lo que hay dentro.
protected-contents-why = Es una carpeta conocida (Documentos, Vídeos...): se puede añadir su contenido, pero no la carpeta en sí.
details-in-basket = En la cesta
dry-run-banner = Modo simulación: no se borra nada
cleaned-badge = ≈ Aproximado hasta reescanear
cleaned-tip = Has limpiado desde el análisis: los tamaños se han actualizado, pero los gráficos de tipo y de antigüedad no. Reescanea (F5) para ponerlos al día.
info-name-pagefile = Memoria virtual (pagefile.sys)
info-name-hiberfil = Hibernación (hiberfil.sys)
info-name-swapfile = Intercambio de aplicaciones (swapfile.sys)
dest-permanent = Se borra · se vuelve a crear solo

# Recomendaciones
risk-safe-tag = Seguro
recs-title-safe = Hasta { $size } en candidatos de bajo riesgo (se comprueba al limpiar)
recs-title-none = No hay nada seguro que limpiar ahora
recs-subtitle-review = Y hay { $size } más que conviene que mires tú.
recs-clean-safe = Limpiar lo seguro ({ $size })
recs-safe-hint = Ya marcado. Se recrea solo o se reinstala.
recs-review-hint = Sin marcar. Ábrelo y decide tú.
recs-info-hint = Lo gestiona Windows. Te explicamos qué es.
recs-unavailable-title = No disponible
recs-unavailable-line = { $name }: { $detail }
recs-unavailable-link = la carpeta es un enlace (junction) a otra ubicación; Tesela no limpia a través de enlaces.
recs-unavailable-changed = la carpeta cambió mientras se comprobaba; vuelve a comprobar.
recs-unavailable-outside = la carpeta temporal está redirigida fuera de los datos locales del usuario; Tesela no limpia carpetas redirigidas.
recs-none = En esta carpeta no hay sitios de limpieza conocidos. Analiza una unidad entera para ver recomendaciones.
recs-review-none = Aquí no hay nada que revisar.
recs-info-none = No hay archivos de este tipo en lo analizado.
recs-include = Incluir { $name }
recs-browser-cache = Caché de { $name }
recs-blocked = { $name } está abierto: ciérralo para liberar hasta { $size }
recs-recheck = Volver a comprobar
recs-view = Ver
recs-empty-bin = Vaciar
rule-browsers-name = Caché de navegadores
rule-browsers-why = Copias de webs que el navegador vuelve a descargar. Tus contraseñas, historial y sesiones no se tocan.
rule-user-temp-why = Archivos temporales sin modificar en la última semana (más de 7 días). Los recientes se dejan por si un instalador los está usando.
rule-windows-temp-why = Archivos temporales de Windows sin modificar en la última semana (más de 7 días). Los recientes se dejan por si un instalador los está usando.
rule-downloads-why = Archivos tuyos: míralos y elige. Nada de aquí se marca solo.
rule-recycle-bin-why = Vaciarla es definitivo. Solo se vacía lo que hay al confirmar; lo que entre después no se toca.
rule-wer-reports-why = Informes que Windows guarda cuando un programa falla, ya enviados o en cola. Sirven para diagnosticar el fallo y el soporte de un programa puede pedirlos: si los borras, los pierdes. Solo se tocan los de más de 7 días. Van a la Papelera y no se marcan solos: cada uno guarda un fallo concreto y no se puede regenerar.
rule-crash-dumps-why = Volcados de memoria de programas que se cerraron por un fallo (carpeta CrashDumps, que suele existir solo si se activó esa función de Windows). Pueden ocupar mucho y son lo que pide un desarrollador para entender el fallo: si los borras, los pierdes. Solo se tocan los de más de 7 días. Van a la Papelera y no se marcan solos: cada uno guarda un fallo concreto y no se puede regenerar.

# Confirmación
confirm-title = Revisa antes de limpiar
confirm-subtitle =
    { $n ->
        [one] { $count } elemento · { $size } · comprobamos cada ruta otra vez justo antes de borrar
       *[other] { $count } elementos · { $size } · comprobamos cada ruta otra vez justo antes de borrar
    }
confirm-elevated = Tesela se está ejecutando como administrador; por seguridad la limpieza solo funciona sin elevar. Ciérrala y ábrela normalmente.
confirm-no-log = No se pudo crear el registro de la limpieza, así que no se ha hecho nada. Comprueba que se puede escribir en la carpeta de datos de Tesela (en tu carpeta de datos locales) y vuelve a intentarlo. La simulación sigue funcionando sin registro.
confirm-empty = No hay nada que limpiar con esta cesta.
confirm-dry-banner = Modo simulación: no se borrará ni se moverá nada; solo se registra lo que se haría.
confirm-reconfirm = Algo cambió desde que lo confirmaste (lo que cabe en la Papelera). Revisa de nuevo antes de limpiar.
confirm-group-permanent = Se borra y se vuelve a crear sola
confirm-permanent-note = Esta parte no se puede deshacer, pero Windows y tus programas la vuelven a crear cuando la necesitan.
confirm-upto = hasta { $size }
confirm-group-bin = Vaciar la Papelera
confirm-bin-note =
    { $n ->
        [one] Se elimina definitivamente el { $count } elemento que hay ahora (unos { $size }). Lo que entre después no se toca.
       *[other] Se eliminan definitivamente los { $count } elementos que hay ahora (unos { $size }). Lo que entre después no se toca.
    }
confirm-group-recycle = Va a la Papelera de reciclaje · puedes recuperarlo
confirm-recycle-note = Son archivos tuyos; no se recrean. El espacio se libera de verdad al vaciar la Papelera.
confirm-your-file = Tu archivo
confirm-in-folder = en { $folder }
confirm-no-space = no liberará espacio
confirm-more =
    { $n ->
        [one] y { $count } más
       *[other] y { $count } más
    }
confirm-group-unfit = Se eliminará definitivamente porque no se puede enviar a la Papelera
confirm-unfit-capacity-hint = Para poder enviarlo a la Papelera: abre las propiedades de la Papelera de reciclaje y fija un tamaño máximo para la unidad.
confirm-unfit-check = Entiendo que esto no se podrá recuperar
confirm-unfit-note = Sin marcar la casilla, estos elementos no se tocan.
unfit-no-bin = la unidad no tiene Papelera
unfit-nuke = la Papelera de la unidad está desactivada
unfit-too-big = supera el tamaño de la Papelera
unfit-path-long = la ruta es demasiado larga para la Papelera
unfit-policy = una directiva del sistema impide usar la Papelera
unfit-unknown = no se pudo comprobar si cabe en la Papelera
unfit-capacity-unknown = la Papelera de la unidad no tiene un tamaño máximo configurado: no hay garantía de que quepa
confirm-hardlink =
    { $n ->
        [one] { $count } archivo está compartido con otro nombre: no liberará espacio ({ $size })
       *[other] { $count } archivos están compartidos con otro nombre: no liberarán espacio ({ $size })
    }
confirm-cloud = Sincronizado con OneDrive: borrarlo también lo quita de la nube.
confirm-protected =
    { $n ->
        [one] Se omitirá { $count } elemento protegido.
       *[other] Se omitirán { $count } elementos protegidos.
    }
confirm-protected-size =
    { $n ->
        [one] Se omitirá { $count } elemento protegido ({ $size }).
       *[other] Se omitirán { $count } elementos protegidos ({ $size }).
    }
confirm-rejected =
    { $n ->
        [one] { $count } elemento ya no se puede limpiar (cambió, no existe o está protegido).
       *[other] { $count } elementos ya no se pueden limpiar (cambiaron, no existen o están protegidos).
    }
reject-missing = ya no existe
reject-denied = sin permiso
reject-identity = no se puede verificar
reject-reparse = es un enlace
reject-reparse-ancestor = un enlace en su ruta
reject-outside-local = la carpeta temporal está redirigida fuera de los datos locales del usuario
reject-protected = protegido
reject-untouchable = no se puede borrar entero
reject-incomplete =
    { $n ->
        [one] no se pudo leer entera: { $count } elemento sin acceso
       *[other] no se pudo leer entera: { $count } elementos sin acceso
    }
reject-other = error del sistema
reject-not-local = no es una unidad local
confirm-see-all = Ver la lista completa
confirm-hide-all = Ver menos
confirm-go-both = Liberar hasta { $free } y mover { $bin } a la Papelera
confirm-go-free = Liberar hasta { $free }
confirm-go-bin = Mover { $bin } a la Papelera
confirm-go-free-zero = Limpiar
confirm-go-dry = Simular la limpieza

# Ejecución y resumen
run-title = Limpiando…
run-title-dry = Simulando…
run-phase-preparing = Preparando…
run-phase-bin = Vaciando la Papelera…
run-phase-delete = Eliminando…
run-phase-recycle = Moviendo a la Papelera…
run-phase-verify = Comprobando cada elemento…
run-progress = { $done } de { $total } elementos · { $count } entradas revisadas
run-close-blocked = Espera a que termine o cancela: cerrar ahora dejaría la limpieza a medias.
run-cancelling = Cancelando: termina lo que ya empezó y no toca nada más.
sum-freed = Cambio medido del espacio libre
sum-freed-hint = Es lo que cambió el espacio libre de la unidad durante la limpieza: otros programas que escriban o borren a la vez también lo modifican. Lo eliminado y lo enviado a la Papelera se cuentan aparte.
sum-dry = Simulación: esto es lo que se habría eliminado (estimación)
sum-permanent = Eliminado definitivamente
sum-recycled = Movido a la Papelera
sum-bin-purged = Papelera vaciada
sum-bin-items =
    { $n ->
        [one] { $count } elemento
       *[other] { $count } elementos
    }
sum-before = Antes
sum-now = Ahora
sum-used-pct = { $used } usados · { $pct }
sum-bin-card = Otros { $size } están en la Papelera de reciclaje
sum-bin-card-note = Ese espacio se libera de verdad cuando la vacías.
sum-bin-card-all = La Papelera tiene { $total } en total y se vacía entera.
sum-empty-now = Vaciar ahora ({ $size })
sum-open-bin = Abrir la Papelera
sum-back = Volver al mapa
sum-in-use =
    { $n ->
        [one] Se saltó { $count } archivo en uso. Nada se ha forzado.
       *[other] Se saltaron { $count } archivos en uso. Nada se ha forzado.
    }
sum-denied =
    { $n ->
        [one] { $count } elemento sin permiso: se saltó.
       *[other] { $count } elementos sin permiso: se saltaron.
    }
sum-browser-open = { $name } sigue abierto: ciérralo para liberar hasta { $size } más.
sum-changed =
    { $n ->
        [one] { $count } elemento cambió desde la confirmación y no se tocó.
       *[other] { $count } elementos cambiaron desde la confirmación y no se tocaron.
    }
sum-protected =
    { $n ->
        [one] { $count } elemento protegido se respetó.
       *[other] { $count } elementos protegidos se respetaron.
    }
sum-reparse =
    { $n ->
        [one] { $count } enlace no se siguió (no se entra en los enlaces).
       *[other] { $count } enlaces no se siguieron (no se entra en los enlaces).
    }
sum-young =
    { $n ->
        [one] { $count } archivo reciente se dejó (menos de 7 días).
       *[other] { $count } archivos recientes se dejaron (menos de 7 días).
    }
sum-shared =
    { $n ->
        [one] { $count } archivo compartido con otro nombre no liberó espacio.
       *[other] { $count } archivos compartidos con otro nombre no liberaron espacio.
    }
sum-unexpected =
    { $n ->
        [one] { $count } elemento se eliminó definitivamente, o su contenido cambió o perdió una parte, mientras Windows lo procesaba, en vez de ir a la Papelera (por ejemplo porque no cabía). Revisa el registro de la limpieza y la Papelera.
       *[other] { $count } elementos se eliminaron definitivamente, o su contenido cambió o perdió una parte, mientras Windows los procesaba, en vez de ir a la Papelera (por ejemplo porque no cabían). Revisa el registro de la limpieza y la Papelera.
    }
sum-shell-error = La Papelera de Windows devolvió un error (0x{ $code }). Cada elemento que no se pudo enviar se comprobó: lo que sigue intacto en su sitio no se tocó y el resto se cuenta aparte.
sum-recycle-stopped =
    { $n ->
        [0] Se detuvo el envío a la Papelera: Windows eliminó definitivamente un elemento, o su contenido cambió o perdió una parte, al procesarlo. Por precaución no se tocó nada más.
        [one] Se detuvo el envío a la Papelera: Windows eliminó definitivamente un elemento, o su contenido cambió o perdió una parte, al procesarlo. Por precaución no se envió { $count } elemento más: no se tocó.
       *[other] Se detuvo el envío a la Papelera: Windows eliminó definitivamente un elemento, o su contenido cambió o perdió una parte, al procesarlo. Por precaución no se enviaron { $count } elementos más: no se tocaron.
    }
sum-recycle-stopped-unverified =
    { $n ->
        [0] Se detuvo el envío a la Papelera: no se pudo comprobar qué pasó con un elemento. Por precaución no se tocó nada más.
        [one] Se detuvo el envío a la Papelera: no se pudo comprobar qué pasó con un elemento. Por precaución no se envió { $count } elemento más: no se tocó.
       *[other] Se detuvo el envío a la Papelera: no se pudo comprobar qué pasó con un elemento. Por precaución no se enviaron { $count } elementos más: no se tocaron.
    }
sum-recycle-stopped-changed =
    { $n ->
        [0] Se detuvo el envío a la Papelera: un elemento cambió justo antes de enviarlo y no se envió. Por precaución no se tocó nada más.
        [one] Se detuvo el envío a la Papelera: un elemento cambió justo antes de enviarlo y no se envió. Por precaución no se envió { $count } elemento más: no se tocó.
       *[other] Se detuvo el envío a la Papelera: un elemento cambió justo antes de enviarlo y no se envió. Por precaución no se enviaron { $count } elementos más: no se tocaron.
    }
sum-recycle-stopped-declined =
    { $n ->
        [0] Se detuvo el envío a la Papelera: Windows no pudo enviar un elemento y se comprobó que lo dejó intacto en su sitio. Por precaución no se tocó nada más.
        [one] Se detuvo el envío a la Papelera: Windows no pudo enviar un elemento y se comprobó que lo dejó intacto en su sitio. Por precaución no se envió { $count } elemento más: no se tocó.
       *[other] Se detuvo el envío a la Papelera: Windows no pudo enviar un elemento y se comprobó que lo dejó intacto en su sitio. Por precaución no se enviaron { $count } elementos más: no se tocaron.
    }
sum-no-room =
    { $n ->
        [one] { $count } elemento no se envió porque no hay sitio en la Papelera: vacíala o libera espacio. No se tocó.
       *[other] { $count } elementos no se enviaron porque no hay sitio en la Papelera: vacíala o libera espacio. No se tocaron.
    }
sum-recycled-other =
    { $n ->
        [one] { $count } elemento cambió justo antes de enviarlo: a la Papelera llegó algo distinto de lo que aprobaste. Está en la Papelera y puedes restaurarlo.
       *[other] { $count } elementos cambiaron justo antes de enviarlos: a la Papelera llegó algo distinto de lo que aprobaste. Está en la Papelera y puedes restaurarlo.
    }
sum-unverified =
    { $n ->
        [one] No se pudo comprobar qué pasó con { $count } elemento: revísalo en su sitio y en la Papelera.
       *[other] No se pudo comprobar qué pasó con { $count } elementos: revísalos en su sitio y en la Papelera.
    }
sum-not-recycled =
    { $n ->
        [one] { $count } elemento no se movió a la Papelera (en uso, sin permiso u otro motivo de Windows): no se tocó.
       *[other] { $count } elementos no se movieron a la Papelera (en uso, sin permiso u otro motivo de Windows): no se tocaron.
    }
sum-skipped-unfit =
    { $n ->
        [one] { $count } elemento que no cabe en la Papelera no se tocó.
       *[other] { $count } elementos que no caben en la Papelera no se tocaron.
    }
sum-bin-failed =
    { $n ->
        [one] La Papelera no se pudo vaciar del todo: { $count } elemento sigue ahí.
       *[other] La Papelera no se pudo vaciar del todo: { $count } elementos siguen ahí.
    }
sum-bin-changed =
    { $n ->
        [one] { $count } elemento de la Papelera cambió desde la confirmación y no se tocó.
       *[other] { $count } elementos de la Papelera cambiaron desde la confirmación y no se tocaron.
    }
sum-bin-replaced =
    { $n ->
        [one] { $count } elemento de la Papelera cambió justo antes de borrarlo: Windows borró otro objeto con su nombre. No se pudo evitar y no se puede deshacer.
       *[other] { $count } elementos de la Papelera cambiaron justo antes de borrarlos: Windows borró otros objetos con sus nombres. No se pudo evitar y no se puede deshacer.
    }
sum-log-failed = No se pudo escribir el registro de la limpieza, así que se detuvo en el siguiente punto seguro y no se tocó nada más. Lo que ya estaba hecho se queda y puede no constar entero en el registro.
sum-log-truncated = El registro de la limpieza llegó a su tamaño máximo y dejó de anotar: lo que se hizo después no consta en él.
sum-bin-unverifiable =
    { $n ->
        [one] { $count } elemento de la Papelera está en una unidad cuyo sistema de archivos no permite verificarlo (FAT o exFAT) y no se tocó.
       *[other] { $count } elementos de la Papelera están en una unidad cuyo sistema de archivos no permite verificarlos (FAT o exFAT) y no se tocaron.
    }
sum-bin-with-links =
    { $n ->
        [one] { $count } elemento de la Papelera contiene enlaces (o no se pudo comprobar su contenido) y no se vació: se dejó donde estaba.
       *[other] { $count } elementos de la Papelera contienen enlaces (o no se pudo comprobar su contenido) y no se vaciaron: se dejaron donde estaban.
    }
sum-bin-unlisted =
    { $n ->
        [one] { $count } elemento de la Papelera tiene los datos dañados, no se pudo leer y no se incluyó al vaciar.
       *[other] { $count } elementos de la Papelera tienen los datos dañados, no se pudieron leer y no se incluyeron al vaciar.
    }
sum-cancelled = Cancelaste la limpieza: lo que ya estaba hecho se queda.
sum-diverge = "Eliminado definitivamente" y el cambio medido del espacio libre no coinciden: puede ser por enlaces duros, archivos que otro programa tenía abiertos, escrituras de otros programas o cambios del propio Windows.

ago-days =
    { $n ->
        [one] hace { $count } día
       *[other] hace { $count } días
    }
settings-clear-history = Borrar el historial y los registros de limpieza
home-last-clean = Última limpieza: { $ago } · cambio medido del espacio libre: { $size }
