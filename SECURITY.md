# Política de seguridad

## Cómo reportar una vulnerabilidad

**No publiques los detalles de una vulnerabilidad en una issue pública.** Hay dos canales:

1. **Aviso privado de GitHub (preferido):** pestaña **Security** del repositorio, botón **Report a vulnerability**
   ([github.com/wandoth1/tesela/security/advisories/new](https://github.com/wandoth1/tesela/security/advisories/new)).
   Solo lo ven los mantenedores. *Si el botón no aparece, el reporte privado todavía no está activado en el repositorio: usa el
   canal 2.*
2. **Canal de respaldo (visible desde hoy):** abre una [issue](https://github.com/wandoth1/tesela/issues/new) **SIN detalles
   técnicos** (sin pasos, sin capturas, sin datos): solo di que quieres reportar un problema de seguridad y pide un canal
   privado. Un mantenedor te responderá en la issue con cómo seguir, y la issue se borrará o se dejará sin detalles.

Cuando el aviso se confirme, se prepara el arreglo en privado, se publica una versión corregida y después el aviso (con crédito
a quien lo reportó, si lo quiere).

Para reportar, describe: versión de Tesela, qué ocurre, cómo reproducirlo y qué impacto tiene. **No incluyas datos personales**
(nombres de carpetas o archivos reales, tu nombre de usuario, capturas de tu disco): para reproducir un fallo basta una carpeta
de prueba o el modo `--demo`, que usa datos sintéticos.

## Qué cuenta como vulnerabilidad

Tesela **borra archivos** y se instala sin administrador, así que interesa sobre todo:

- Que Tesela borre o mueva algo que no estaba en la confirmación (fuera del conjunto aprobado, o siguiendo un enlace, junction
  o carpeta sustituida a mitad de la operación).
- Que una carpeta protegida (Windows, Archivos de programa, `AppData`, el propio Tesela) se pueda limpiar.
- Elevación de privilegios, o ejecución de código, a través del instalador MSI, del zip portable o de los ejecutables.
- Que un paquete publicado no coincida con su `SHA256SUMS.txt` o con su attestation de procedencia, o que lleve datos de la
  máquina que lo construyó.
- Que la aplicación envíe información fuera del equipo (no debe: no tiene telemetría ni cuentas).

## Versiones con soporte

Solo la **última versión publicada** recibe correcciones de seguridad (hoy, la serie 0.x).

## Plazos

Se intenta acusar recibo en pocos días y dar un primer análisis en dos semanas. Es un proyecto personal sin garantías de
servicio; si un aviso queda sin respuesta, se puede insistir en el mismo aviso o issue.
