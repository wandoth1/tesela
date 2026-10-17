# Hoja de ruta de producto

Fecha: 2026-10-16. Sustituye a la tabla de fases de `PLAN.md` §4 para todo lo que va después de la v0.1. El diseño
técnico sigue en `PLAN.md`.

Origen: un estudio de diferenciación con fuentes y dos revisiones externas independientes del plan. Las afirmaciones
sobre la competencia hay que volver a comprobarlas antes de publicarlas.

## 1. Tesis

Ver qué ocupa el disco ya está resuelto, y bien: WizTree, WinDirStat, TreeSize, y el Sensor de almacenamiento y las
recomendaciones de Windows. Tesela no compite en velocidad de escaneo ni en tipos de gráfico.

Tesela compite en **ayudar a decidir cómo recuperar espacio**: qué es cada cosa, qué pasa si la quitas, cuál es la
forma documentada de hacerlo y qué ocurrió de verdad después. Alguien puede usar otra herramienta para encontrar el
espacio y Tesela para decidir qué hacer con él.

**Posicionamiento:**
> Tesela explica lo que reconoce, orienta hacia métodos documentados y solo ofrece acciones de limpieza bajo
> condiciones verificadas.

Lema corto: *"Entiende tu disco antes de borrar."*

Nunca "sin riesgo", "sin miedo" ni "demostrado seguro". La identidad de un archivo demuestra qué objeto se abrió, no
que borrarlo sea inocuo.

## 2. Segmento inicial (hipótesis por validar)

- **Quién adopta:** la persona que ayuda de forma habitual a familiares o conocidos con problemas de espacio en Windows
  domésticos, cuando Configuración y el Explorador no lo aclaran. Compara, instala, repite y recomienda.
- **Quién se beneficia:** quien tiene la unidad C: llena, a menudo en un portátil con SSD pequeño.
- **Fuera por ahora:**
  - equipos corporativos gestionados (políticas, permisos y soporte);
  - el segmento de desarrolladores, que es secundario y se deja para después de validar.

## 3. Reglas de producto

1. **Reconocer no es autorizar.** Una ruta, un archivo ancla, `CACHEDIR.TAG` o un nombre de carpeta solo producen una
   explicación. Nunca preseleccionan nada ni levantan una protección.
2. **Tesela no ejecuta comandos elevados ni herramientas de terceros por el usuario.** Explica, abre la página exacta de
   Configuración (`ms-settings:`) o muestra y copia el comando oficial con sus efectos y su aviso de administrador.
3. **Tres estados visibles:** *desconocido*, *conocido pero no recomendable* y *candidato con condiciones*. Lo desconocido
   nunca recibe una explicación inventada.
4. **Dos resultados distintos:**
   - "he evitado un daño";
   - "he resuelto la falta de espacio".

   Se cuentan por separado. Un producto para la unidad C: llena necesita sobre todo el segundo.
5. **Salidas que liberan espacio sin que Tesela borre lo delicado:**
   - liberar la copia local de OneDrive (el archivo sigue en la nube);
   - mover datos a otra unidad;
   - ir a Aplicaciones instaladas;
   - usar los métodos oficiales de Windows.
6. **Sin telemetría**, tampoco "anónima".
7. **Nunca limpiar elevado.** Una lectura elevada, si llega, va en un ayudante aparte, opcional y de solo lectura, con
   capacidades mínimas.

## 4. Catálogo de conocimiento

Las explicaciones y recomendaciones son **datos** (TOML embebido) con un validador en CI. Cada entrada lleva:

- fuente primaria (fabricante o Microsoft Learn), fecha de revisión y caducidad **proporcional a su ritmo de cambio**;
- precondiciones, efectos, acción (borrar, Papelera, herramienta oficial, abrir Configuración o no tocar),
  recuperación y **condiciones para abstenerse**;
- pruebas positivas y negativas.

Además:

- Cuando una entrada caduca deja de recomendar cualquier acción, **también el comando para copiar**. La explicación se
  puede seguir mostrando con un aviso.
- Una entrada con acción destructiva necesita confianza "verificada".
- El registro de cada limpieza guarda la **versión de la regla** y sus precondiciones, además de la identidad del
  archivo.
- Una contribución al catálogo se revisa como si fuera código: comprueba la interpretación, el alcance y la
  aplicabilidad, no solo que tenga fuente.
- El valor no es el texto, que se puede copiar. Es el conjunto:

  > reconocer el contexto → recomendar lo apropiado → abstenerse cuando faltan datos → comprobar qué ocurrió

  respaldado por un corpus de casos y regresiones.

## 5. Fase de validación (3 meses)

Antes de ampliar funciones hay que demostrar que Tesela resuelve problemas de espacio mejor que lo que ya existe.

### Mes 1: perímetro de seguridad y evidencia pública

- **Publicar la v0.1.2.** Lleva las correcciones de las auditorías y textos honestos.
- **Batería pública de seguridad** en una máquina virtual desechable, publicada con cada release:
  - archivos centinela en las zonas protegidas;
  - carreras, junctions y sustituciones;
  - disco casi lleno: sin espacio para instalar o para escribir el registro;
  - fallos de escritura del registro, contenido que cambia, y recuperación con conflictos.
- **Comportamiento con el disco casi lleno:** modo de solo diagnóstico y orientación. Nunca se salta el registro.
- **Firma:** solicitar SignPath y publicar en winget, en paralelo y sin depender de ninguno de los dos.

### Mes 2: pocos recorridos completos a partir de casos reales

- Reunir los casos siguientes de unas 10 personas que ayudan a otros, con consentimiento y con los datos personales
  mínimos.
- Construir **6–8 recorridos completos**, elegidos por la frecuencia observada y porque de verdad liberan espacio. No
  hacen falta 15 fichas.
- El panel "¿Qué es esto?", el historial y las exclusiones mínimas existen para servir a esos recorridos.
- **Medir antes de decidir:** cuántos casos de "Sistema y reservado" no se pueden resolver sin ver lo que el usuario no
  puede leer. Si son muchos, el ayudante elevado de solo lectura deja de ser algo para más adelante y pasa al núcleo.
  Primero se evalúa reutilizar el enumerador actual; la MFT solo si hace falta.

### Mes 3: piloto sin ayuda del autor

- Unos 30 casos, entre escenarios reproducibles y casos reales. No deben ser solo los casos con los que se diseñaron
  las reglas.
- **Referencia de comparación:**
  - para quien ayuda, WinDirStat + Configuración;
  - para el usuario general, Explorador + Configuración.
- Se mide el recorrido completo: instalar, entender, actuar y verificar.

### Métrica de decisión

> **Porcentaje de problemas de espacio resueltos correctamente, sin intervención del autor y sin acciones dañinas.**

- **Objetivo:** fijado de antemano. O bien recuperar el espacio que necesitaba una tarea, o bien completar un
  diagnóstico concreto. "No toques esto" no cuenta como espacio liberado.
- **Umbral orientativo:** al menos un 70 % de casos resueltos y 20 puntos por encima de la alternativa habitual, con la
  batería de seguridad superada.
- **Señal complementaria:** quien ayuda vuelve a elegir Tesela en otro caso sin que se lo pidan.
- **Si no hay ventaja:** no se añaden más funciones. Se recorta el alcance, se revisa el segmento o se convierte el
  catálogo y sus pruebas en un componente para otras herramientas.
- **Si hay un incidente de seguridad:** se detiene la función afectada. Una buena media de utilidad no lo compensa.

Treinta casos son un piloto para orientar el proyecto, no una certificación estadística.

## 6. Después de la validación (solo si la métrica sale bien)

- **v0.2, "Entender antes de borrar":**
  - los recorridos validados en el panel "¿Qué es esto?", con su ruta oficial;
  - historial y Deshacer con contrato;
  - archivos grandes y antiguos con contexto;
  - exclusiones mínimas;
  - un informe sencillo para compartir con quien ayuda, que oculta rutas y nombres sensibles;
  - accesibilidad básica comprobada: teclado, foco, escalado y lectura de las decisiones.
- **Contrato de Deshacer:**
  - solo deshace lo que hizo Tesela y que sigue en la Papelera;
  - verifica por identidad y nunca sobrescribe;
  - da el resultado elemento a elemento: restaurado, ya no está, conflicto o cambió;
  - lo hecho por Windows, OneDrive u otras herramientas queda fuera y se dice así.
- **Ver lo inaccesible:** un ayudante elevado de solo lectura que nombra lo que el usuario no puede leer, sin ofrecer
  borrarlo. Llega antes o después según lo medido en el mes 2.
- **Segmento técnico** (más adelante): cachés de desarrollo mediante su comando oficial, con su alcance exacto (por
  ejemplo, `docker builder prune` en lugar de `docker system prune` si solo se quiere la caché de compilación; `uv cache
  clean` o `prune` según el caso) y carpetas de proyecto con condiciones de abstención (cambios sin confirmar,
  enlaces, sin lockfile).
- **Aplazado hasta tener evidencia:** duplicados, reescaneo incremental (USN), ARM64 publicado y exportación completa.

## 7. Anti-objetivos

- Competir en velocidad de escaneo o en tipos de gráfico.
- Limpiador de registro, "optimizar", puntuación de salud del PC, desinstalador.
- Limpieza automática o programada.
- Ejecutar DISM, powercfg, docker, uv u otras herramientas en nombre del usuario.
- Explicaciones generadas por IA sin fuente, o reglas importadas de catálogos de terceros.
- Prometer cifras de regeneración o "sin riesgo".
- Recomendar algo por su tamaño o su extensión (por ejemplo, un `.db-wal` puede contener datos confirmados de una base
  de datos).

## 8. Métricas complementarias

- **Batería de seguridad:**
  - 0 centinelas tocados y 0 cambios fuera de lo aprobado (requisito de publicación, no una estimación de seguridad
    real);
  - además, que las operaciones permitidas **funcionan**. Una herramienta que siempre se abstiene también pasa las
    pruebas negativas.
- **Catálogo:**
  - recomendaciones operativas activas dentro de su vigencia;
  - cobertura útil del espacio que de verdad ocupa;
  - tiempo dedicado a revisarlo. No se mide "0 caducadas", que invita a renovar fechas por encima.
- **Estudios con usuarios de 5 en 5:** sirven para encontrar problemas de comprensión, no para afirmar que los errores
  peligrosos son raros. Las preguntas críticas (por ejemplo, "¿esto borra también la copia de la nube?") tienen que
  acertarse **todas**, no solo de media.
- **Descargas e issues:** son señales auxiliares, no un denominador de daños ni de utilidad.

## 9. Riesgos y tareas abiertas

- **Antivirus:** un antivirus heurístico puede desconfiar del comportamiento de un limpiador (borrado masivo, uso de la
  Papelera por COM). Ya ocurrió con el binario de **pruebas** al ejecutarlo en bucle. Antes de anunciar la limpieza hay
  que probar `tesela.exe` con los antivirus más comunes (Microsoft Defender y otros dos o tres) y, si alguno lo marca,
  reportar el falso positivo al fabricante. La firma de código ayuda a la reputación, pero no lo resuelve por sí sola.
- **Canal Microsoft Store (experimento, no decidido):** es la única forma de evitar del todo el aviso de SmartScreen,
  porque Microsoft firma el paquete. Exige MSIX. Una app de escritorio `runFullTrust` puede leer todo el disco, pero
  Windows **virtualiza las escrituras en AppData**, que es justo donde están las cachés y los temporales que se limpian.
  Antes de decidir hay que comprobar en una prueba si, con la capacidad restringida `unvirtualizedResources`, la limpieza
  de AppData funciona exactamente igual que con el MSI, y si la Tienda la acepta para un limpiador. Hasta entonces el MSI
  sigue siendo el canal principal.
- **Firma:** SignPath no está garantizado (pide reputación verificable y política de privacidad). Hay que confirmar la
  disponibilidad y el coste reales de las alternativas antes de contar con ellas.
- **Responsabilidad:** la licencia no da inmunidad automática. Antes de promocionar mucho la limpieza conviene una
  revisión legal acotada de las promesas, la licencia y la gestión de incidentes.
- **Mantenimiento del catálogo:** las fuentes cambian. La caducidad es un mecanismo de revisión, no una prueba de que
  algo siga siendo cierto.
- **El umbral de "poco espacio":** se combina el porcentaje con el espacio absoluto y el contexto. El recorrido "necesito
  espacio" está siempre accesible y nunca se convierte en una alarma de "salud".
