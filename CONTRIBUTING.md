# Contribuir a ViceModVI

La unidad de trabajo es una tarea `MF-XX` de [PENDING.md](PENDING.md). Cada contribución incluye lo necesario para que otra persona la revise y reproduzca sin consultar al autor.

## Preparar el entorno

1. Trabaja desde una copia local del repo y elige una tarea con las dependencias que necesites. El alcance inicial está en [README.md](README.md).
2. Guarda los contenedores de muestra y los resultados de análisis fuera del repo. Para las pruebas automatizables usa muestras sintéticas pequeñas con un resultado conocido. Comprueba los casos reales con una copia a la que tengas acceso.
3. Para el primer caso real, usa un `core_restofworld.rpf` de Xbox Series y su `index.tsv` si lo tienes. Para relaciones entre archivos añade el TTPF de la textura que estés comprobando. Pasa todas las rutas mediante argumentos o configuración local.
4. Cuando una tarea introduzca el primer ejecutable o biblioteca de su área, deja documentados en el mismo cambio las dependencias y el comando exacto para compilar y reproducir el ejemplo. El proyecto arranca con documentación y la herramienta de compilación se fija junto a la primera implementación que la requiera.

## Implementar

- Para catálogo o lectores, comprueba primero qué campos ya aportan nuestros TSV y qué campos están en los manifiestos de exportación disponibles. Reutiliza esas salidas con un importador o una interfaz sencilla antes de implementar otro escáner.
- Declara el origen de cada dato. Conserva offset, longitud, identificador nativo y tipo de evidencia cuando estén disponibles. Etiqueta los alias y las asociaciones candidatas. Conserva identificadores de 64 bits como cadenas hexadecimales en JSON para evitar pérdidas de precisión en clientes que usan números de coma flotante.
- Para edición, conserva el original como entrada de solo lectura y escribe los reemplazos en el proyecto o en una copia nueva. Incluye en la evidencia qué ocurre al reabrir esa copia.
- Si incorporas código de terceros, comprueba su licencia antes de mezclarlo con el proyecto y conserva las atribuciones y avisos exigidos. Para interoperar con herramientas externas, documenta la versión, el formato de entrada y la interfaz utilizada.
- Mantén la lógica de formatos de Xbox en su backend. Coloca las funciones compartidas de catálogo, proyecto y vista en código independiente de la plataforma. Documenta el perfil y versión que probaste.

## Entregar un cambio que se pueda revisar

Si usas Git, crea una rama identificable, por ejemplo `mf-03-material-textura`. Referencia la tarea en el PR y copia esta plantilla en la descripción.

```markdown
Tarea: MF-XX
Qué cambia:
Entrada: plataforma, versión, nombre y huella de la muestra si está disponible
Reproducción: herramientas, versiones y comando o pasos completos
Resultado esperado:
Resultado observado:
Cómo se midió: recuentos, casos positivos y casos rechazados o ambiguos
Límites pendientes:
```

Usa ejemplos y rutas relativas que otra persona pueda adaptar. Para muestras de juego, adjunta solo métricas, pasos y salidas de diagnóstico que se puedan compartir. Mantén fuera del repositorio los RPF, TTPF, ejecutables, claves, recursos extraídos, catálogos generados con datos del juego y resultados binarios locales.

## Revisar y cerrar

1. Comprueba los criterios de aceptación de la tarea con su reproducción documentada.
2. Revisa un caso positivo y, si aplica, un rango inválido, un formato sin soporte o una relación ambigua.
3. Para exportación o escritura, vuelve a abrir el archivo generado y registra qué campos o contenido cotejaste. Distingue la lectura desde el visor de una prueba dentro del juego.
4. Actualiza la tarea y documenta los casos pendientes que podrían cambiar el resultado. Si surge trabajo independiente, añádelo a [PENDING.md](PENDING.md) con su evidencia esperada.
