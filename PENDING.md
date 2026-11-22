# Trabajo pendiente

Selecciona una tarea que puedas cerrar con la evidencia indicada. Las tareas están ordenadas por dependencia. El equipo puede preparar exportadores y pruebas de formatos en paralelo mientras se acuerdan los contratos de catálogo y referencias.

| ID | Tarea | Depende de | Estado |
| --- | --- | --- | --- |
| MF-01 | Catálogo y lectura por offset | - | Pendiente |
| MF-02 | Identificación y exportación de modelos y texturas | MF-01 para la integración | Pendiente |
| MF-03 | Relación entre materiales y texturas | MF-01, MF-02 | Pendiente |
| MF-04 | Visor y proyecto de reemplazos | MF-02, MF-03 | Pendiente |
| MF-05 | Escritura experimental en copia TTPF | MF-02 para validar formato, MF-04 para integrar | Pendiente |
| MF-06 | Escritura RPF9 y distribución de cambios | Evidencia propia por formato y plataforma | Investigación |

## MF-01 · Catálogo y lectura por offset

Importar un TSV de RPF y manifiestos con nombres, tipos y localizadores. Usar como muestra un inventario de `core_restofworld.rpf`. Separar el rango de bytes de la identidad del recurso. Para un RPF, guardar el contenedor, offset, longitud y segmentos cuando existan. Para un TTPF, guardar el contenedor, la clave nativa y el índice local cuando esté disponible. Asociar el catálogo a una huella comprobable del archivo. Filtrar por origen, tipo, nombre o hash.

**Se da por hecha cuando** una segunda apertura puede recuperar una entrada usando el catálogo guardado sin volver a escanear el contenedor. Registrar bytes leídos para la operación y probar el rechazo de un rango fuera de archivo o de un catálogo que corresponda a otra copia.

## MF-02 · Identificar y exportar

Conectar lectores existentes a entradas del catálogo. Comparar la salida con manifiestos existentes. Validar el tipo con la estructura del recurso. Conservar `desconocido` cuando solo haya un bloque zlib o falle la validación. Para texturas, registrar dimensiones, formato, mips fuente y mips exportados. Para modelos, registrar mallas, materiales y localizadores. Exponer la lectura individual en una interfaz reutilizable por el visor.

**Se da por hecha cuando** se puede exportar una textura y una malla de referencia a partir del catálogo, reabrir cada salida y comprobar dimensiones y mips disponibles, o recuento de vértices y caras, respectivamente. La evidencia debe identificar el decodificador usado y distinguir salidas completas de parciales.

## MF-03 · Resolver materiales y texturas

Partir de una malla ya exportada. Usar las referencias del material y contrastar con texturas embebidas, diccionarios RPF y pools TTPF. Conservar los identificadores completos, su procedencia y la evidencia que justifica cada vínculo. Un mismo hash puede dar varios candidatos. Cuando falte una referencia suficiente, mostrar los candidatos y el motivo de la incertidumbre.

**Se da por hecha cuando** se puede seleccionar el material de una malla de referencia y llegar a su textura difusa con una asociación reproducible. Añadir un caso ambiguo que no se resuelva automáticamente. Medir vínculos confirmados, ambiguos y sin resolver por separado.

## MF-04 · Visor y proyecto local

Mostrar catálogo, previsualización de textura, selección de malla y material, y exportación individual. Aceptar un reemplazo, indicar diferencias de dimensiones, formato y mips cuando proceda, permitir comparar antes y después y guardar ese estado fuera del archivo fuente. Registrar la identidad de la entrada que se reemplaza.

**Se da por hecha cuando** un segundo colaborador puede abrir la muestra, editar una textura, verla sobre el modelo, cerrar y reabrir el proyecto, y revertir el cambio. Comprobar las huellas de los archivos originales antes y después del flujo.

## MF-05 · Copia TTPF con un reemplazo

Comenzar con una textura y un perfil de almacenamiento ya legible. Documentar las condiciones para aceptar el reemplazo. Escribir en una copia nueva, actualizar los campos que requiera el perfil y volver a abrirla con un lector. Verificar la textura modificada y una entrada vecina. Rechazar perfiles sin soporte en el escritor.

**Se da por hecha cuando** la copia conserva una estructura legible, se puede volver a exportar la textura esperada y la entrada vecina conserva su contenido. Registrar la compatibilidad en juego como pendiente hasta tener una comprobación en la plataforma y versión exactas.

## MF-06 · Escritura RPF9 y distribución

Documentar qué metadatos de RPF9 quedan sin interpretar y qué necesita un escritor para producir una copia válida. Diseñar los paquetes de reemplazos cuando se sepa qué mecanismo de aplicación admite la plataforma.

**Se da por hecha por plataforma** cuando se puede aplicar y revertir un cambio reproducible sobre una copia y comprobar que el juego lee el resultado. Cada plataforma debe tener su propia prueba y registro de compatibilidad.

## Cómo declarar progreso

Actualiza el estado de la tarea en el mismo cambio que aporta evidencia. Indica la versión de la muestra, el comando o los pasos y el resultado esperado y observado. Usa recuentos con denominador y unidad: recursos detectados, modelos decodificados y referencias resueltas son medidas distintas. Los bytes escaneados no se contabilizan como nombres, rutas ni recursos recuperados.

[Visión del proyecto](README.md) · [Guía de contribución](CONTRIBUTING.md)
