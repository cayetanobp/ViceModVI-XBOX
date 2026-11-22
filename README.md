# ViceModVI - Xbox

ViceModVI es una herramienta de escritorio para explorar, previsualizar, exportar y editar recursos de juego locales. La primera edición se centra en archivos RPF9 y TTPF de Xbox Series. El flujo está pensado para modders y parte de una copia legible de los archivos.

**Estado actual:** este repositorio arranca con la especificación del producto. El visor, el catálogo y la escritura todavía están por implementar aquí. Hay inventarios TSV y lectores previos que sirven como entradas o referencias para el desarrollo.

## Flujo de trabajo

1. Abrir un RPF, un TTPF o una carpeta de trabajo. Importar un inventario existente o generar el catálogo al abrir por primera vez.
2. Buscar por nombre recuperado, tipo, hash, archivo u offset. Mostrar si el nombre procede del recurso, de una referencia o de un alias generado por la aplicación.
3. Previsualizar una textura o un modelo. Al seleccionar un material, mostrar las texturas que usa, dónde se guardan y qué otros recursos se sabe que las referencian.
4. Exportar el recurso elegido y volver a importar un reemplazo. Comparar original y edición sobre la malla, guardar el proyecto y poder revertir sus cambios.
5. Cuando exista un escritor validado, generar una copia modificada. La compatibilidad en juego se registra por versión y plataforma tras una prueba real.

## Alcance de la primera versión

| Área | Entrega inicial | Ampliación |
| --- | --- | --- |
| Catálogo | Texturas y modelos, búsqueda e índice persistente | Audio, animaciones, VFX y otros tipos cuando exista un lector |
| Vista previa | Imagen, canales y mips disponibles. Modelo con material y texturas resueltas | Variantes y ensamblaje de componentes cuando sus referencias se puedan demostrar |
| Exportación | Una textura en DDS o PNG según el caso y una malla en un formato interoperable | Exportación por lotes y materiales completos |
| Edición | Importar un reemplazo a un proyecto local y comparar antes y después | Copia TTPF con reemplazo validado. Escritura RPF9 y paquetes en hitos separados |

DDS es la salida para conservar formato y mips cuando el lector puede entregarlos. PNG es una salida de edición para la imagen seleccionada. Cada exportación debe poder rastrearse al archivo y recurso original.

## Cómo se reparte el trabajo

- **Catálogo.** Importa inventarios y manifiestos generados por las herramientas disponibles. Persiste localizadores y tipos sin almacenar los archivos del juego en el repo.
- **Lectores.** Leen la entrada seleccionada por offset o identificador y devuelven datos listos para exportar o visualizar. Un recurso puede necesitar varios segmentos.
- **Referencias.** Vincula modelos, materiales y texturas por identificador o por referencias observadas. Devuelve evidencia y candidatos ambiguos para que el visor los pueda distinguir.
- **Proyecto y edición.** Guarda reemplazos y sus metadatos aparte. Un backend de escritura independiente podrá generar copias cuando el formato esté validado.
- **Interfaz.** Comparte el catálogo y el modelo de proyecto entre plataformas. Xbox es el primer backend. Los nombres previstos para las otras ediciones son ViceModVI - PS5 y ViceModVI para PC. La cobertura de cada plataforma se publica por separado.

## Límites de los datos

El TSV identifica rangos detectados durante el escaneo. Los nombres y tipos recuperados se incorporan desde los recursos y manifiestos. La tabla original de rutas de RPF9 sigue pendiente. Un bloque comprimido puede ser solo una parte de un recurso. Los hashes de TTPF se conservarán como identificadores cuando falten nombres legibles.

El primer objetivo verificable es abrir **un modelo local**, resolver **una textura difusa** desde su material, exportarla, importar un reemplazo y guardar un proyecto que vuelva a mostrar el cambio al abrirlo.

[Trabajo pendiente](PENDING.md) · [Guía de contribución](CONTRIBUTING.md)

## Licencia

El código propio de ViceModVI se publica bajo [GNU GPL v3 o posterior](LICENSE) (SPDX: GPL-3.0-or-later). Los archivos del juego conservan sus propios derechos.
