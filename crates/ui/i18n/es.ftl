# Textos de la interfaz en español. Todo lo que ve el usuario sale de aquí: no hay cadenas de interfaz en el código.
# Hay un test que exige que `es.ftl` y `en.ftl` tengan exactamente los mismos identificadores y que el código solo use
# identificadores que existen.

app-name = Tesela
settings = Ajustes
settings-theme = Tema
settings-language = Idioma
about = Acerca de Tesela
about-version = Versión { $v }
about-description = Analizador de espacio en disco para Windows. En esta versión solo lee: no borra ni mueve nada.
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
error-scan-panicked = un hilo del análisis falló de forma inesperada
error-scan-startup-lost = el arranque del análisis se interrumpió
error-scan-cancelled = el análisis se canceló
error-open-explorer = No se pudo abrir el Explorador: { $error }
error-reveal-invalid = la ruta no es válida
error-reveal-parse = Windows no reconoce la ruta ({ $detail })
error-reveal-open = el Explorador no pudo mostrarla ({ $detail })

# Escaneo
scan-title = Mientras analizamos tu disco…
scan-counts = { $files } archivos · { $size } · { $time }
scan-counts-of = { $files } archivos · { $size } de { $total } · { $time }
scan-stop = Parar y ver lo que hay
scan-progress-label = Progreso del análisis: { $pct } %
scan-known-title = Sitios donde suele haber archivos temporales
scan-known-sum = { $size } en sitios conocidos
scan-known-note = De momento solo se muestra cuánto ocupan; qué es seguro limpiar llega en la próxima versión.
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
info-hiberfil = Archivo de hibernación de Windows. Si no usas la hibernación se puede desactivar con powercfg /h off; Tesela no ejecuta nada.
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
