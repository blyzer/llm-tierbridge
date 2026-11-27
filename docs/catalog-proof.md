# Prueba de catálogo normalizado para el gateway (models.dev + modelparameters.dev + gateways omp)

Fecha de descarga de las fuentes externas: **2026-11-26T08:32:56Z** (UTC). Todo lo de abajo se regenera con `python3 /tmp/gateway-proof/build_report.py` a partir de `out/results.json`; ninguna cifra está escrita a mano.

> **Estado.** Hecho: (1) esquema, (2) descargas, (3) cinco adaptadores + validación contra el esquema, (4) emparejamiento y comparación con cifras reales. **Pendiente** (marcado `PENDIENTE`): `merge.py` que aplique la tabla de precedencia y emita registros `model` con procedencia (el esquema ya los define, pero no se han generado ni validado instancias `model`), comparación referencia-contra-referencia (models.dev vs modelparams.dev), y re-auditoría manual de T3 tras el último ajuste de la regla.

## 1. Resumen ejecutivo

- Los tres gateways suman **1762 ids únicos**. **1264 (71.7%)** tienen match en models.dev o modelparams.dev; **498 (28.3%)** no tienen ninguno. De los sin match, cerca de la mitad parecen no-chat (imagen/STT/TTS/embeddings; heurística), p. ej. 221 de 491 en la caché.
- modelparams.dev solo cubre 650 de esos ids y **casi todo lo que cubre ya está en models.dev** (por gateway, "al menos uno" == cobertura de models.dev; en la unión solo 1 id casa únicamente en modelparams.dev). Aporta lo que models.dev no tiene: parámetros aceptados, estado `retired/deprecated`, `shutdownOn`, `replacement`.
- **El riesgo real no es el emparejamiento, es que los gateways rellenan valores por defecto.** En la caché de cliproxyapi, el 90.3% de las filas tiene exactamente `(contextWindow, maxTokens) = (272000, 16384)` y el 90.8% la escalera de esfuerzo `[low, medium, high]`. Esos valores coinciden con models.dev en 0% (contexto) de los casos comparables; los valores no-placeholder coinciden en 68%. En el live de easycliproxy el par `(128000, 32768)` cubre el 39.6%.
- El coste es el campo que más falta en el gateway: `omp models easycliproxy --json` devuelve `pricingStatus: unknown` en **1699 de 1699** filas y dsh en **51 de 51**; models.dev puede rellenar 910 de easycliproxy.

## 2. Método

### 2.1 Fuentes y permisos

| Fuente | Cómo | Fecha / revisión | Permiso |
|---|---|---|---|
| models.dev | `GET https://models.dev/api.json` (1 petición, 6.8 MB) | 2026-11-26T08:32:56Z | Repositorio `sst/models.dev`, licencia MIT (`raw/models.dev.LICENSE`). Es un JSON público y estático. |
| modelparameters.dev | `https://modelparameters.dev/` responde **308 → https://modelparams.dev/**. Se usó `GET /api/v1/models.json` (1 petición, 1.2 MB, 510 modelos) | `generatedAt` 2026-11-24T08:22:46.486Z | `robots.txt`: `User-agent: *  Allow: /` con la nota "AI agents welcome" y `/api/v1/*.json` rastreable a propósito. `llms.txt` publica la API como interfaz para máquinas. Repositorio `mnfst/modelparams.dev` con licencia MIT (`raw/mp.github.LICENSE`). No existe página de términos (`/terms`, `/license`, `/about` dan 404). **Permitido; se descargó el catálogo completo en una sola petición, sin rastrear páginas HTML.** |
| `~/.omp/agent/cliproxyapi-models.json` | lectura local | `fetchedAt` = 2026-11-26T05:20:01Z | local |
| `omp models easycliproxy --json --no-extensions` | binario `…/Tern/omp/versions/19.2.1` | ejecutado 2026-11-26T08:32:56Z | local |
| `omp models dsh --json --no-extensions` | idem | idem | local |
| (sonda) roster empaquetado de omp | `zstd -d fixtures/llm-oracle/catalog/models.json.zst` | árbol de trabajo | local; solo se usa como sonda de circularidad, no como fuente de verdad |

Crudos en `/tmp/gateway-proof/raw/` (con `fetch-date.txt`, cabeceras HTTP y `sha256` truncado dentro de `source.origin` de cada observación).

### 2.2 Lo aprendido del crate `omp-catalog`

- No hay un esquema "models.dev" literal en el crate. El roster llega de `models.json` de pi (`scripts/import_v1_models.py`), que ya usa nombres camelCase de `SourceModelRecord` (`crates/catalog/src/compile.rs:239`): `id, name, provider, identity{class,family,revision}, api, baseUrl, reasoning, input[], output[], cost{input,output,cacheRead,cacheWrite,longContext}, contextWindow, maxTokens, thinking{mode,efforts,defaultLevel,effortMap,effortBudgets,requiresEffort,…}, deprecated, supportsTools`. El importador descarta explícitamente `timeBased` (precios por franja) y otros campos de v1.
- Los límites tipados son `ModelLimits {context_window, maximum_input_tokens, maximum_output_tokens, maximum_batch}`; el precio en runtime son enteros `NanoUsd` por `PriceUnit`, con `PriceTier.prompt_tokens_above`.
- La procedencia ya existe: `ProvenanceKind {bundled, discovered, configured}`, `EvidenceConfidence {verified, declared, inferred, unknown}`, `ProvenanceSource {kind, origin, revision, confidence, observed_at_ms}` y `ModelProvenance {sources, updated_at_ms, blocked_until_ms, deprecated}`. **Lo que no existe es procedencia por campo**, ni `retired`/`replacement`/`shutdownOn`, ni parámetros aceptados.
- Reglas de identidad reutilizadas para el emparejamiento (`compat/taxonomy/_discovery.kdl`): `trailing-marker`, `billing-variant-suffix`, `wrapper-prefix "duo-chat-"`, `synthetic-prefix "hf:"`, `canonical-family-token` (`gemini2` → `gemini-2`). El crate dice que `-max`, `-none`, `-extra-high` NO deben fusionarse; aquí tampoco se quitan.
- `docs/research/providers-signin.md` trata de proveedores y OAuth, no de este esquema; solo cita models.dev una vez (el roster de `modular` viene de models.dev).

## 3. Esquema propuesto

Archivo: `/tmp/gateway-proof/schema/gateway-model.schema.json` (JSON Schema 2020-12). Dos formas con `recordType` discriminante:

- **`observation`**: lo que UNA fuente dice de UN modelo. Lo desconocido es `null`, nunca `0`/`false`. Lo emiten los adaptadores.
- **`model`**: registro fusionado que sirve el gateway: valores ganadores + `references[]` (qué entradas de referencia casaron y con qué regla) + `provenance{sources[], fields{ruta → FieldProvenance}}`.

| Campo | Tipo | Origen del nombre |
|---|---|---|
| `provider`, `id`, `name`, `kind` | string / enum | `SourceModelRecord` (+ `selector` solo en la salida de omp) |
| `canonicalId` | `vendor/model` | models.dev `canonical_model_id` (nuevo) |
| `contextWindow`, `maxInputTokens`, `maxTokens` | entero ≥1 o null | `contextWindow`/`maxTokens` de `SourceModelRecord`; `maxTokens` = `ModelLimits.maximum_output_tokens` |
| `cost{status, input, output, cacheRead, cacheWrite, tiers[]}` | USD por Mtok | `SourceCost`; `tiers[].promptTokensAbove` = `PriceTier.prompt_tokens_above`; **`status` nuevo**: `known/free/unknown` (un 0 con `pricingStatus: unknown` NO es gratis) |
| `input[]`, `output[]` | enum text/image/audio/video/pdf | `SourceModality` |
| `reasoning` | bool o null | `SourceModelRecord.reasoning` |
| `thinking{mode, efforts[], canDisable, defaultLevel, effortMap, min/maxBudgetTokens, dynamicBudget}` | efforts: enum `off,minimal,low,medium,high,xhigh,max` | `SourceThinking` + `ReasoningEffort` (`none`→`off`) |
| `lifecycle{status: live/preview/deprecated/retired/unknown, replacement, shutdownOn}` | | crate solo tiene `deprecated: bool`; ampliado con modelparams.dev |
| `acceptedParams[]{path,type,group,values,range,default,conditional}` | | nuevo (modelparams.dev `params[]`) |
| `authType` | api_key/subscription/… | modelparams.dev (variantes separadas) |
| `provenance.fields[ruta]` | `{source, rule, confidence, conflict, alternates[{source,value,confidence}]}` | `EvidenceConfidence` reutilizado; el resto es nuevo |

**Validación real:** los adaptadores emiten **19 382 observaciones** (`models.dev` 10 049, `modelparams.dev` 510, `cliproxyapi-cache` 1 704, `omp-live:easycliproxy` 1 699, `omp-live:dsh` 51, `omp-bundled` 5 369) y `validate.py` (jsonschema 4.26, Draft 2020-12) da **0 inválidas**. La validación encontró tres datos reales que el esquema rechazaba: precio negativo `-1000000` en `openrouter/auto` del roster de omp (centinela de "precio variable"), `budget_tokens.min = -1` en models.dev (presupuesto dinámico) y la clave `ultra` de `thinkingLevelMap` en la caché (fuera del enum del crate). Los adaptadores los mapean a `unknown`/`dynamicBudget`/descartado.

## 4. Regla de emparejamiento de ids (explícita)

Entrada: el id de cable del gateway tal cual (p. ej. `gw/model-2`, `gw/qwen/qwen3-coder:free`). Se prueban, **en este orden y se para en el primero que acierta**, cuatro niveles contra el índice de la referencia (id y `canonical_model_id` de cada entrada, más `provider/id`):

1. **T1 exacto-completo**: `lower(id)` == `lower(id_ref)`, == `lower(canonical_ref)` o == `provider/id_ref`. Quita antes el prefijo sintético `hf:`.
2. **T2 exacto-hoja**: la **hoja** (último segmento tras `/`, sin `duo-chat-`) es igual a la hoja de una entrada (o de su canonical). Ignora prefijos de ruta del gateway (`dva/`, `aug/`, `our-free-model/`, `devin/`…).
3. **T3 normalizado**: en la hoja del gateway se (a) separa versión pegada tras token de familia (`gemini2.5`→`gemini-2.5`), (b) `4.5`→`4-5`, `_`→`-`, (c) se quitan marcadores finales `[-:](customtools|cloud|exacto|nitro|original|optimized|nvfp4|fp8|fp4|bf16|int8|int4|thinking|minimal|low|medium|high|xhigh|free|contributor)` repetidamente. **Los marcadores solo se quitan del lado del gateway**, nunca de la referencia (ver auditoría).
4. **T4 sin fecha**: T3 más quitar `-YYYYMMDD`, `-YYYY-MM-DD` o `-YYMM` final.

Varias entradas pueden casar (la misma hoja existe en decenas de revendedores). El valor de referencia por campo se elige así: entradas **de primera parte** (el vendor del `canonical_model_id` es el propio proveedor; o, sin canonical, el segmento de vendor del id del gateway) → si coinciden entre sí, ese valor; si no, mayoría estricta; si no hay primera parte → mayoría estricta entre revendedores; si no hay mayoría, **`ambiguous`** y el campo no se compara.

**Precisión (auditoría manual, 25 pares aleatorios por nivel, semilla fija, `out/results.json › audit`):** T1 25/25 plausibles; T2 25/25 plausibles (ambos sorteos anteriores al último ajuste de T3); T3 antes del ajuste 24/25: un falso positivo `gw/vision → vispark/vision-medium` causado por quitar `-medium` también del lado de la referencia. Se corrigió (solo se quitan marcadores del lado del gateway) y las coberturas de abajo son las posteriores a la corrección. **PENDIENTE: re-auditar T3 con el nuevo sorteo** (tras el cambio quedan 295 pares T3 y 6 T4 contra models.dev). T4 es el nivel más débil y de volumen mínimo. Riesgo conocido de T2/T3: casar un gateway `X-1m` (contexto 1M) con la base `X` (200k) hace que el contexto de la referencia sea incorrecto; el sufijo `-1m` no está en la lista de marcadores precisamente por eso.

## 5. Cifras de emparejamiento

### 5.1 Contra models.dev (por nivel)

| Gateway | N | T1 exacto-completo | T2 exacto-hoja | T3 normalizado | T4 sin-fecha | **con match** | sin match |
|---|--:|--:|--:|--:|--:|--:|--:|
| cliproxyapi (caché) | 1704 | 810 | 248 | 152 | 3 | **1213 (71.2%)** | 491 |
| easycliproxy (live) | 1699 | 816 | 253 | 143 | 3 | **1215 (71.5%)** | 484 |
| dsh (live) | 51 | 0 | 47 | 0 | 0 | **47 (92.2%)** | 4 |

### 5.2 Contra modelparams.dev (por nivel)

| Gateway | N | T1 exacto-completo | T2 exacto-hoja | T3 normalizado | T4 sin-fecha | **con match** | sin match |
|---|--:|--:|--:|--:|--:|--:|--:|
| cliproxyapi (caché) | 1704 | 259 | 142 | 217 | 17 | **635 (37.3%)** | 1069 |
| easycliproxy (live) | 1699 | 257 | 140 | 204 | 17 | **618 (36.4%)** | 1081 |
| dsh (live) | 51 | 0 | 14 | 6 | 0 | **20 (39.2%)** | 31 |

### 5.3 Cobertura combinada

| Gateway | N | models.dev | modelparams.dev | ambos | al menos uno | **ninguno** |
|---|--:|--:|--:|--:|--:|--:|
| cliproxyapi (caché) | 1704 | 1213 | 635 | 635 | 1213 | **491 (28.8%)** |
| easycliproxy (live) | 1699 | 1215 | 618 | 618 | 1215 | **484 (28.5%)** |
| dsh (live) | 51 | 47 | 20 | 19 | 48 | **3 (5.9%)** |
| **ids únicos (unión)** | 1762 | 1263 | 650 | – | 1264 | **498 (28.3%)** |

### 5.4 Los que NO casan

| Gateway | sin match en ninguna | de ellos parecen no-chat (heurística de regex) | de ellos parecen chat |
|---|--:|--:|--:|
| cliproxyapi (caché) | 491 | 221 | 270 |
| easycliproxy (live) | 484 | 224 | 260 |
| dsh (live) | 3 | 0 | 3 |

Prefijos más frecuentes entre los sin match (caché): `aihorde` 164, `dva` 60, `auto` 36, `(no prefix)` 30, `aug` 15, `devin` 15, `tllm` 11, `elevenlabs` 8. `aihorde/*` son modelos de imagen de AI Horde: el campo `kind` de `omp models … --json` dice `chat` en **todas** las filas (1699 de 1699), así que `kind` del gateway no sirve para filtrar. Muestra de los que parecen chat y no casan (easycliproxy): `gw/reasoning`, `gw/model-7`, `gw/claude-sonnet-5-max`, `gw/model-9`, `model-10`, `gw/model-11`, `gw/claude-opus-4-7-max`, `gw/model-13`, `gw/model-14`, `gw/model-15`, `gw/model-16`, `gw/model-17`. Son alias del propio gateway (`auto/*`, `dva/…-1m`, `no-think/…`, `tllm/…`) o modelos aún no listados en las referencias.

## 6. Discrepancias de campos (gateway vs models.dev)

"Placeholder" = valor que se repite en ≥20% de las filas de la fuente (par `(contextWindow,maxTokens)` o escalera de esfuerzos). Se excluye de coinciden/discrepan y se cuenta aparte. "Cerca" = diferencia relativa ≤6% (1 000 000 vs 1 048 576 = 4.6%).

**Validación de la heurística de placeholder** (¿coincide el valor con la referencia?):

| Gateway | Campo | filas placeholder: coinciden | filas no-placeholder: coinciden |
|---|---|--:|--:|
| cliproxyapi (caché) | contextWindow | 0/956 (0.0%) | 95/140 (67.9%) |
| cliproxyapi (caché) | maxTokens | 26/876 (3.0%) | 65/139 (46.8%) |
| cliproxyapi (caché) | efforts | 71/595 (11.9%) | 63/101 (62.4%) |
| easycliproxy (live) | contextWindow | 23/246 (9.3%) | 713/848 (84.1%) |
| easycliproxy (live) | maxTokens | 16/209 (7.7%) | 589/796 (74.0%) |
| easycliproxy (live) | efforts | 14/419 (3.3%) | 82/237 (34.6%) |

Los placeholders casi nunca coinciden con la referencia y los demás sí, por lo que se tratan como "desconocido".

### 6.1 Contexto, máx. salida, coste, razonamiento (contra models.dev)

| Gateway | Campo | comparables (ambos tienen valor) | coinciden | cerca (≤6%) | **discrepan** | gw placeholder (excluido) | gw sin dato / ref sí | ref ambigua |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| cliproxyapi (caché) | contextWindow | 140 | 95 | 10 | **35** | 1070 | 0 | 3 |
| cliproxyapi (caché) | maxTokens | 139 | 65 | 10 | **64** | 1070 | 0 | 4 |
| cliproxyapi (caché) | cost(input,output) | 368 | 359 | 0 | **9** | 0 | 553 | 199 |
| cliproxyapi (caché) | reasoning | 1210 | 874 | 0 | **336** | 0 | 0 | 3 |
| easycliproxy (live) | contextWindow | 848 | 713 | 70 | **65** | 297 | 0 | 63 |
| easycliproxy (live) | maxTokens | 796 | 589 | 16 | **191** | 297 | 0 | 103 |
| easycliproxy (live) | cost(input,output) | 0 | 0 | 0 | **0** | 0 | 910 | 197 |
| easycliproxy (live) | reasoning | 1212 | 1007 | 0 | **205** | 0 | 0 | 3 |
| dsh (live) | contextWindow | 31 | 26 | 4 | **1** | 16 | 0 | 0 |
| dsh (live) | maxTokens | 31 | 23 | 2 | **6** | 16 | 0 | 0 |
| dsh (live) | cost(input,output) | 0 | 0 | 0 | **0** | 0 | 45 | 2 |
| dsh (live) | reasoning | 47 | 44 | 0 | **3** | 0 | 0 | 0 |

Direcciones (discrepan): contexto easycliproxy gw>ref 34 / gw<ref 31; máx. salida easycliproxy gw>ref 104 / gw<ref 87. **No hay sesgo único**: el gateway no siempre subestima. Un ejemplo observado: `openai/gpt-5.2-codex` 272 000 en el gateway frente a 400 000 en models.dev (posible límite de plan; no verificado).

**Razonamiento.** Discrepancias con `gw=true, ref=false`: caché 323 (de ellas 122 son no-chat por salida o por regex) y easycliproxy 157 (87 no-chat). Los ejemplos son TTS, embeddings y vídeo marcados `reasoning: true`. Además la caché marca `reasoning=true` en 1 691 de 1 704 filas (99.2%): también es un valor por defecto. Contra modelparams.dev, las discrepancias de razonamiento son todas `gw=false, ref=true` (caché 5, easycliproxy 32).

### 6.2 Contra modelparams.dev (máx. salida = `range.max` del parámetro de longitud; esfuerzos = enum de `reasoning_effort`)

| Gateway | Campo | comparables (ambos tienen valor) | coinciden | cerca (≤6%) | **discrepan** | gw placeholder (excluido) | gw sin dato / ref sí | ref ambigua |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| cliproxyapi (caché) | maxTokens | 28 | 1 | 12 | **15** | 560 | 0 | 3 |
| cliproxyapi (caché) | reasoning | 515 | 510 | 0 | **5** | 0 | 0 | 0 |
| cliproxyapi (caché) | efforts | 45 | 25 | 0 | **20** | 556 | 3 | 10 |
| easycliproxy (live) | maxTokens | 137 | 74 | 18 | **45** | 65 | 0 | 9 |
| easycliproxy (live) | reasoning | 501 | 469 | 0 | **32** | 0 | 0 | 0 |
| easycliproxy (live) | efforts | 78 | 29 | 0 | **49** | 301 | 14 | 33 |
| dsh (live) | maxTokens | 2 | 0 | 0 | **2** | 8 | 0 | 0 |
| dsh (live) | reasoning | 11 | 9 | 0 | **2** | 0 | 0 | 0 |
| dsh (live) | efforts | 2 | 0 | 0 | **2** | 10 | 2 | 0 |

modelparams.dev solo declara `range.max` en 113 de 510 entradas, por eso casi todo es "ref sin dato".

### 6.3 Diez ejemplos por campo (muestra aleatoria con semilla fija; `out/examples.json` tiene los demás)

**contextWindow**

| Gateway | id gateway | valor gateway | valor referencia | entrada models.dev | regla | base ref |
|---|---|---|---|---|---|---|
| cliproxyapi (caché) | `gpt-6.1-sol` | 500000 | 1050000 | `steadyrouter:gpt-6.1-sol` | T1 | first-party |
| easycliproxy (live) | `gw/model-18` | 1048576 | 262144 | `llmtr:thinkingmachines/inkling` | T3 | first-party |
| dsh (live) | `gw/nvidia/nemotron-3.5-lightning:free` | 262144 | 1000000 | `kilo:nvidia/nemotron-3.5-lightning:free` | T2 | majority |
| cliproxyapi (caché) | `gpt-6-astra` | 272000 | 1050000 | `opper:gpt-6-astra` | T1 | first-party |
| easycliproxy (live) | `gw/anthropic/claude-opus-4.6` | 168000 | 1000000 | `cloudflare-ai-gateway:anthropic/claude-opus-4.6` | T2 | majority |
| cliproxyapi (caché) | `grok-4.20-0309-reasoning` | 2000000 | 1000000 | `xai:grok-4.20-0309-reasoning` | T1 | majority |
| easycliproxy (live) | `openai/gpt-5.1-codex-max` | 272000 | 400000 | `azure:gpt-5.1-codex-max` | T1 | majority |
| cliproxyapi (caché) | `gpt-6.1-terra` | 500000 | 1050000 | `azure:gpt-6.1-terra` | T1 | first-party |
| easycliproxy (live) | `mistralai/mistral-small-3.2-24b-instruct` | 256000 | 128000 | `aimlapi:mistralai/mistral-small-3.2-24b-instruct` | T1 | majority |
| cliproxyapi (caché) | `gw/glm-5-2` | 200000 | 1000000 | `mistral:glm-5-2` | T2 | majority |

**maxTokens**

| Gateway | id gateway | valor gateway | valor referencia | entrada models.dev | regla | base ref |
|---|---|---|---|---|---|---|
| cliproxyapi (caché) | `gw/flint-40b-free` | 32768 | 65536 | `opencode:flint-40b-free` | T2 | single |
| easycliproxy (live) | `gpt-image-2` | 10000 | 16384 | `azure:gpt-image-2` | T1 | single |
| dsh (live) | `gw/nvidia/nemotron-3.5-lightning:free` | 262144 | 65536 | `kilo:nvidia/nemotron-3.5-lightning:free` | T2 | majority |
| cliproxyapi (caché) | `grok-4.3` | 65536 | 30000 | `opper:grok-4.3` | T1 | majority |
| easycliproxy (live) | `qwen/qwen3-coder-30b-a3b-instruct` | 235929 | 65536 | `siliconflow:Qwen/Qwen3-Coder-30B-A3B-Instruct` | T1 | majority |
| dsh (live) | `gw/stepfun-ai/step-5-preview` | 1000000 | 65536 | `opencode:step-5-preview-free` | T2 | first-party |
| cliproxyapi (caché) | `gw/grok-4-7` | 64000 | 500000 | `kenari:grok-4-7` | T2 | majority |
| easycliproxy (live) | `deepseek-r1` | 8192 | 32768 | `qiniu-ai:deepseek-r1` | T1 | majority |
| dsh (live) | `gw/meta/muse-glimmer-30b` | 32768 | 131072 | `llmtr:muse-glimmer-30b-tr` | T2 | majority |
| cliproxyapi (caché) | `gw/model-26` | 64000 | 262144 | `azure:fw-nemotron-lightning-3.5-30b-a3b` | T3 | first-party |

**coste (input, output) USD/Mtok** (todas las discrepancias; solo la caché tiene coste conocido)

| Gateway | id gateway | valor gateway | valor referencia | entrada models.dev | regla | base ref |
|---|---|---|---|---|---|---|
| cliproxyapi (caché) | `openai/gpt-5.1-codex` | [1.07, 8.5] | [1.25, 10] | `opencode:gpt-5.1-codex` | T1 | majority |
| cliproxyapi (caché) | `openai/gpt-5-codex` | [1.07, 8.5] | [1.25, 10] | `opencode:gpt-5-codex` | T1 | majority |
| cliproxyapi (caché) | `gpt-5.1-codex` | [1.07, 8.5] | [1.25, 10] | `azure:gpt-5.1-codex` | T1 | majority |
| cliproxyapi (caché) | `gw/deepseek-flash-latest` | [0.0975, 0.39] | [0.3, 1.2] | `kilo:~gw/deepseek-flash-latest` | T2 | majority |
| cliproxyapi (caché) | `model-28` | [1.2626, 2.5253] | [1.25, 2.5] | `vercel:spacexai/grok-4.20-multi-agent` | T2 | majority |
| cliproxyapi (caché) | `claude-sonnet-4.5` | [3, 15] | [3.96, 19.8] | `hostinger-router:claude-sonnet-4.5` | T1 | single |
| cliproxyapi (caché) | `stepfun/step-3.7-flash` | [0.2, 1.15] | [0.20175, 1.21052] | `stepfun:step-3.7-flash` | T1 | first-party |
| cliproxyapi (caché) | `gw/deepseek-pro-latest` | [0.429, 1.287] | [0.111, 4.44] | `the-grid-ai:deepseek-pro-latest` | T2 | majority |
| cliproxyapi (caché) | `grok-4.20` | [1.42, 2.83] | [1.25, 2.5] | `nano-gpt:x-ai/grok-4.20` | T2 | majority |

**reasoning**

| Gateway | id gateway | valor gateway | valor referencia | entrada models.dev | regla | base ref |
|---|---|---|---|---|---|---|
| cliproxyapi (caché) | `google/text-embedding-005` | True | False | `vercel:google/text-embedding-005` | T1 | single |
| easycliproxy (live) | `fish-audio/s1` | True | False | `vercel:fish-audio/s1` | T1 | single |
| dsh (live) | `gw/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free` | False | True | `kilo:nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free` | T2 | majority |
| cliproxyapi (caché) | `alibaba/qwen3-embedding-4b` | True | False | `inference:qwen/qwen3-embedding-4b` | T1 | majority |
| easycliproxy (live) | `grok-imagine-video` | True | False | `xai:grok-imagine-video` | T1 | single |
| dsh (live) | `gw/nvidia/nemotron-3-ultra-550b-a55b` | False | True | `azure:fw-nemotron-3-ultra-nvfp4` | T2 | first-party |
| cliproxyapi (caché) | `tencent/hy-mt2-30b-a3b` | True | False | `aimlapi:tencent/hy-mt2-30b-a3b` | T1 | majority |
| easycliproxy (live) | `nemotron-3-ultra-550b-a55b` | False | True | `kenari:nemotron-3-ultra-550b-a55b` | T1 | majority |
| dsh (live) | `gw/nvidia/nemotron-3-ultra-550b-a55b:free` | False | True | `bothub:nemotron-3-ultra-550b-a55b:free` | T2 | majority |
| cliproxyapi (caché) | `qwen/qwen3-coder-flash` | True | False | `llmtr:qwen/qwen3-coder-flash` | T1 | majority |

## 7. Sonda de circularidad: ¿la salida "live" de omp es independiente de las referencias?

- Misma pasarela, dos vistas: caché 1704 ids, live 1699; en ambas 1675; solo caché 29 (p. ej. `claude-opus-4-6-thinking`); solo live 24 (p. ej. `model-33`).
- Con el mismo id, `contextWindow` difiere en **1595 de 1675** y `maxTokens` en **1603 de 1675**.
- Cuando difieren, el live coincide con models.dev en 668 casos de contexto (la caché en 33) y con el roster empaquetado de omp en 602 (la caché en 12). **Conclusión: la salida live de `omp models` ya mezcla el catálogo empaquetado de omp (que nace de models.dev); no es una observación independiente del gateway.** Hay que tratarla con `confidence: inferred` y no contarla como confirmación de models.dev. La observación independiente sería `GET {modelsUrl}` del gateway (no se hizo: es un servicio local con credenciales).

## 8. Estado de vida (live/retired)

Cuántos modelos del gateway casan con referencias cuyas entradas relevantes están TODAS `deprecated/retired`: caché {"modelparams.dev:all-ref-entries-deprecated": 31, "models.dev:all-ref-entries-deprecated": 82, "modelparams.dev:all-ref-entries-retired": 28}; easycliproxy {"models.dev:all-ref-entries-deprecated": 82, "modelparams.dev:all-ref-entries-retired": 27, "modelparams.dev:all-ref-entries-deprecated": 31}; dsh {"modelparams.dev:all-ref-entries-retired": 2, "models.dev:all-ref-entries-deprecated": 1}. Ejemplos (easycliproxy): `agnes-2.0-flash` (models.dev), `alibaba/qwen-3-14b` (modelparams.dev), `gw/qwen3-max-thinking` (modelparams.dev), `alibaba/qwen3.5-plus` (models.dev), `amazon/nova-premier-v1` (models.dev), `anthropic/claude-opus-4.1` (modelparams.dev).

## 9. Regla de precedencia por campo (propuesta)

Principios: (a) **el gateway define qué filas existen**; las referencias solo enriquecen, nunca crean modelos; (b) un valor es evidencia solo si no es placeholder; (c) primero primera parte, luego mayoría estricta de revendedores, nunca "el primero que aparece"; (d) si hay un valor distinto y no-nulo en otra fuente, el campo lleva `conflict: true` y las alternativas en `alternates` (nada se pierde en silencio).

| Campo | Orden (gana el primero) | Confianza resultante | Por qué (dato de arriba) |
|---|---|---|---|
| `provider`, `id`, `kind` | gateway live | `discovered` | las referencias no saben qué sirve el gateway; `kind` del gateway es siempre `chat`, así que `kind ≠ chat` solo se infiere de models.dev `output` ≠ `["text"]` |
| `name` | gateway live > models.dev 1ª parte | `declared` | nombre cosmético |
| `contextWindow`, `maxInputTokens` | `configured` > gateway live no-placeholder > models.dev 1ª parte > models.dev mayoría estricta > **null** (el placeholder se guarda solo en `alternates`) | `declared` si gateway/1ª parte; `inferred` si mayoría | límite efectivo de la ruta (p. ej. plan 272k) debe ganar al nominal; placeholder coincide ≤9% |
| `maxTokens` | igual que contexto; `modelparams.range.max` solo si models.dev no tiene dato | idem | **PENDIENTE** comparar `range.max` contra `limit.output` para decidir su rango |
| `cost` | `configured` > gateway `status=known` > models.dev 1ª parte (precio de lista) > mayoría estricta > `status: unknown` | `declared` / `inferred` | el gateway devuelve `unknown` en 100% de los live; un 0 nunca se interpreta como gratis |
| `reasoning` | gateway live no-chat-descartado > modelparams (algún param de grupo `reasoning`) > models.dev | `inferred` | 40% de las discrepancias son modelos no-chat con `reasoning:true`; la caché es 99% `true` |
| `thinking.efforts` | gateway live no-placeholder > modelparams enum 1ª parte > models.dev 1ª parte > null | `declared`; `conflict` si gateway ⊃ referencia | la ruta puede aceptar menos/más que el modelo nativo |
| `input` (modalidades) | gateway live > models.dev 1ª parte | `declared` | el gateway suele omitir `pdf` (gw⊂ref en 594 casos) porque es capacidad de la ruta |
| `lifecycle` | modelparams (`status/shutdownOn/replacement`) > models.dev `status` > "listado por el gateway" = `live` | `declared` | solo puede degradar `live`; no hay otra fuente de `retired` |
| `acceptedParams` | modelparams, variante `api_key` de la entrada de 1ª parte | `inferred` si el match es T3/T4 o no 1ª parte | única fuente; el gateway puede no reenviar todos los parámetros |
| `canonicalId` | models.dev `canonical_model_id` (1ª parte/mayoría) | `declared` | agrupa alias de revendedores |

**Cómo se registra la procedencia.** `provenance.sources[]` lista cada fuente con `ProvenanceSource` del crate (`kind`, `origin` con hash de contenido, `revision`, `confidence`, `observedAtMs`) más un `sourceId` estable. `provenance.fields["contextWindow"] = {source, rule, confidence, conflict, alternates[]}` guarda el ganador, el id de la regla (`"limits: gateway-live > ref-first-party > ref-majority"`) y todas las alternativas distintas. `references[]` guarda qué entradas casaron y con qué nivel (`T1..T4`, `firstParty`). Así un valor erróneo es auditable y se puede reproducir el merge sin las descargas, porque las observaciones se conservan en JSONL.

## 10. Limitaciones y pendientes

1. **PENDIENTE `merge.py`**: la tabla de arriba no se ha ejecutado; no hay instancias `model` validadas (solo `observation`).
2. **PENDIENTE** comparación models.dev vs modelparams.dev para las mismas entradas (decide si `range.max` manda sobre `limit.output`).
3. **PENDIENTE** re-auditoría de T3 tras el ajuste; no se verificó ningún valor contra documentación oficial del proveedor: "discrepa" significa "dos fuentes distintas", no "está mal".
4. Los umbrales (20% placeholder, 6% cerca) son decisiones de la prueba; el 20% se validó con las tasas de acierto de la sección 6, pero conviene fijarlos por fuente.
5. Heurística no-chat basada en regex; es indicativa.
6. Fechas del entorno: el reloj del sistema y los datos son de 2026-11; las cifras son del día de descarga.

## 11. Scripts (en `/tmp/gateway-proof/`)

| Archivo | Función |
|---|---|
| `schema/gateway-model.schema.json` | esquema (observation + model) |
| `common.py` | rutas, descriptores de fuente, `RefIndex` (regla T1–T4), `is_first_party` |
| `adapters.py` | 5 adaptadores + sonda → `out/obs.*.jsonl` |
| `validate.py` | valida todo `out/*.jsonl` contra el esquema (usa `.pylib/jsonschema`) |
| `fieldlib.py` | getters, `pick` (1ª parte/mayoría), detección de placeholders |
| `compare.py` | emparejamiento + discrepancias → `out/results.json`, `out/examples.json` |
| `build_report.py` | genera este informe |
| `raw/` | descargas con fecha, cabeceras, licencias, salidas live de `omp models` |

Reproducir: `python3 adapters.py && python3 validate.py && python3 compare.py && python3 build_report.py`.
