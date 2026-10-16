# Política de seguridad

## Cómo reportar una vulnerabilidad

**No abras una issue pública.** Usa los avisos de seguridad privados de GitHub:

1. Ve a la pestaña **Security** del repositorio, [github.com/wandoth1/tesela/security](https://github.com/wandoth1/tesela/security).
2. Pulsa **Report a vulnerability** (o abre directamente
   [github.com/wandoth1/tesela/security/advisories/new](https://github.com/wandoth1/tesela/security/advisories/new)).
3. Describe el problema: versión de Tesela, qué ocurre, cómo reproducirlo y qué impacto tiene.

Solo lo ven los mantenedores. Si el aviso se confirma, se prepara el arreglo en privado, se publica una versión corregida y
después el aviso (con crédito a quien lo reportó, si lo quiere).

**No incluyas datos personales** (nombres de carpetas o archivos reales, tu nombre de usuario, capturas de tu disco): para
reproducir un fallo basta una carpeta de prueba o el modo `--demo`, que usa datos sintéticos.

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
servicio; si un aviso queda sin respuesta, se puede insistir en el mismo aviso.

<!-- PENDIENTE DEL MANTENEDOR: activar "Private vulnerability reporting" en Settings > Code security del repositorio. Hoy
     está DESACTIVADO (la API devuelve enabled=false): sin eso el botón "Report a vulnerability" no aparece y este texto
     remite a algo que no existe. Quitar este comentario cuando esté activado. -->
