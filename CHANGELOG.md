# Cambios

Formato basado en [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/). Las versiones siguen [SemVer](https://semver.org/lang/es/)
(`major.minor.patch`, sin sufijos: un MSI no admite `-rc1`). `release.yml` copia la sección de la versión de la etiqueta a las
notas de la GitHub Release, así que cada versión necesita su sección aquí.

## [0.1.0] - 2026-10-16

Primera versión (MVP). **Sin firmar**: Windows SmartScreen puede avisar (ver el README, "Instalación").

### Añadido

- **Escáner** de solo lectura de unidades y carpetas (walker paralelo con `AllocationSize` real, hard links sin
  sobrecontar, carpetas de la nube sin descargar), y la herramienta de consola `tesela-scan`.
- **Interfaz**: mapa del disco (treemap) y lista sincronizados, detalles, pantalla de escaneo con progreso real y
  veredicto temprano, español e inglés, tema claro y oscuro, accesibilidad (DPI por monitor, foco visible, AccessKit en la
  lista) y modo `--demo` con datos sintéticos.
- **Limpieza segura**: Recomendaciones (temporales, cachés de navegadores, Descargas y Papelera), cesta de revisión,
  confirmación agrupada por destino y resumen con lo liberado de verdad. Borrado por handle sin seguir puntos de reparse,
  envío a la Papelera verificado por identidad, protección de carpetas del sistema, simulación (`--dry-run`) y registro
  de cada limpieza en tu equipo.
- **Instalador MSI por usuario** (WiX 5.0.2): sin administrador, en `%LOCALAPPDATA%\Programs\Tesela`, con acceso en el
  menú Inicio y entrada en Aplicaciones instaladas; una versión nueva sustituye a la anterior y una anterior queda
  bloqueada. Al desinstalar **no** se borran tu configuración ni tu historial. **Zip portable** y `SHA256SUMS.txt`.
- **Releases automáticas**: al empujar una etiqueta `vX.Y.Z` se construyen el MSI y el zip, se atesta su procedencia
  (`gh attestation verify`) y se crea una GitHub Release en **borrador**, que publica una persona tras revisarla.
- Icono de mosaico y datos de versión (producto, editor) en `tesela.exe` y `tesela-scan.exe`.

### Cambiado

- Las pruebas del `cleaner` que usan la Papelera real solo se ejecutan con `TESELA_REAL_RECYCLE_TESTS=1` (el CI de
  GitHub no tiene una Papelera de usuario fiable).
