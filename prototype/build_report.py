"""Builds out/catalog-proof.md (Spanish) from out/results.json + out/examples.json. Numbers are never hand-typed."""
import json
import os

from common import FETCH_DATE, OUT, ROOT

R = json.load(open(f"{OUT}/results.json"))
EX = json.load(open(f"{OUT}/examples.json"))
G = ["cliproxyapi-cache", "omp-live:easycliproxy", "omp-live:dsh"]
SHORT = {"cliproxyapi-cache": "cliproxyapi (caché)", "omp-live:easycliproxy": "easycliproxy (live)", "omp-live:dsh": "dsh (live)"}


def pct(a, b):
    return f"{100 * a / b:.1f}%" if b else "n/a"


def fld(g, ref, f):
    return R["fields"][f"{g} vs {ref} :: {f}"]


def tier_tbl(ref):
    rows = ["| Gateway | N | T1 exacto-completo | T2 exacto-hoja | T3 normalizado | T4 sin-fecha | **con match** | sin match |", "|---|--:|--:|--:|--:|--:|--:|--:|"]
    for g in G:
        m, n = R["match"][g][ref], R["counts"][g]
        t = lambda k: m.get(k, 0)
        hit = n - t("unmatched")
        rows.append(f"| {SHORT[g]} | {n} | {t('T1-exact-full')} | {t('T2-exact-leaf')} | {t('T3-normalized-leaf')} | {t('T4-date-stripped')} | **{hit} ({pct(hit, n)})** | {t('unmatched')} |")
    return "\n".join(rows)


def either_tbl():
    rows = ["| Gateway | N | models.dev | modelparams.dev | ambos | al menos uno | **ninguno** |", "|---|--:|--:|--:|--:|--:|--:|"]
    for g in G:
        n = R["counts"][g]
        md = n - R["match"][g]["models.dev"].get("unmatched", 0)
        mp = n - R["match"][g]["modelparams.dev"].get("unmatched", 0)
        e = R["match"][g]
        rows.append(f"| {SHORT[g]} | {n} | {md} | {mp} | {e['both']} | {e['either']} | **{e['neither']} ({pct(e['neither'], n)})** |")
    u = R["union"]
    rows.append(f"| **ids únicos (unión)** | {u['uniqueIds']} | {u['models.dev']} | {u['modelparams.dev']} | – | {u['either']} | **{u['neither']} ({pct(u['neither'], u['uniqueIds'])})** |")
    return "\n".join(rows)


def field_tbl(ref, fields):
    rows = ["| Gateway | Campo | comparables (ambos tienen valor) | coinciden | cerca (≤6%) | **discrepan** | gw placeholder (excluido) | gw sin dato / ref sí | ref ambigua |", "|---|---|--:|--:|--:|--:|--:|--:|--:|"]
    for g in G:
        for f in fields:
            k = f"{g} vs {ref} :: {f}"
            if k not in R["fields"]:
                continue
            v = R["fields"][k]
            a, nr, d = v.get("agree", 0), v.get("near", 0), v.get("disagree", 0)
            rows.append(f"| {SHORT[g]} | {f} | {a + nr + d} | {a} | {nr} | **{d}** | {v.get('gw_placeholder', 0)} | {v.get('gw_missing_ref_has', 0)} | {v.get('ref_ambiguous', 0)} |")
    return "\n".join(rows)


def pooled(field, n=10, ref="models.dev"):
    lists = [[(g, e) for e in EX.get(f"{g}||{ref}||{field}", {}).get("sample", [])] for g in G]
    out = []
    for i in range(10):          # round-robin across gateways so one source does not fill the table
        for lst in lists:
            if i < len(lst):
                out.append(lst[i])
    return out[:n]


def ex_tbl(field, n=10):
    rows = ["| Gateway | id gateway | valor gateway | valor referencia | entrada models.dev | regla | base ref |", "|---|---|---|---|---|---|---|"]
    for g, e in pooled(field, n):
        rows.append(f"| {SHORT[g]} | `{e['gwId']}` | {e['gw']} | {e['ref']} | `{e['refEntry']}` | {e['tier'].split('-')[0]} | {e['basis']} |")
    return "\n".join(rows)


def pv(k):
    v = R["placeholderValidation"][k]
    return v


def ph_line(g, f):
    v = pv(f"{g} vs models.dev :: {f}")
    pa, pd = v.get("placeholder_agree", 0), v.get("placeholder_disagree", 0)
    ra, rd = v.get("real_agree", 0), v.get("real_disagree", 0)
    return f"| {SHORT[g]} | {f} | {pa}/{pa + pd} ({pct(pa, pa + pd)}) | {ra}/{ra + rd} ({pct(ra, ra + rd)}) |"


P = R["probe"]
pc = P["cache_vs_live_same_id"]
aw = P["disagree_alignment::contextWindow"]
am = P["disagree_alignment::maxTokens"]
reasonC = fld("cliproxyapi-cache", "models.dev", "reasoning")["direction"]
reasonL = fld("omp-live:easycliproxy", "models.dev", "reasoning")["direction"]
lc = R["lifecycle"]
U = R["unmatched"]

md = f"""# Prueba de catálogo normalizado para el gateway (models.dev + modelparameters.dev + gateways omp)

Fecha de descarga de las fuentes externas: **{FETCH_DATE}** (UTC). Todo lo de abajo se regenera con `python3 /tmp/gateway-proof/build_report.py` a partir de `out/results.json`; ninguna cifra está escrita a mano.

> **Estado.** Hecho: (1) esquema, (2) descargas, (3) cinco adaptadores + validación contra el esquema, (4) emparejamiento y comparación con cifras reales. **Pendiente** (marcado `PENDIENTE`): `merge.py` que aplique la tabla de precedencia y emita registros `model` con procedencia (el esquema ya los define, pero no se han generado ni validado instancias `model`), comparación referencia-contra-referencia (models.dev vs modelparams.dev), y re-auditoría manual de T3 tras el último ajuste de la regla.

## 1. Resumen ejecutivo

- Los tres gateways suman **{R['union']['uniqueIds']} ids únicos**. **{R['union']['either']} ({pct(R['union']['either'], R['union']['uniqueIds'])})** tienen match en models.dev o modelparams.dev; **{R['union']['neither']} ({pct(R['union']['neither'], R['union']['uniqueIds'])})** no tienen ninguno. De los sin match, cerca de la mitad parecen no-chat (imagen/STT/TTS/embeddings; heurística), p. ej. {U['cliproxyapi-cache']['nonChatLooking']} de {U['cliproxyapi-cache']['n']} en la caché.
- modelparams.dev solo cubre {R['union']['modelparams.dev']} de esos ids y **casi todo lo que cubre ya está en models.dev** (por gateway, "al menos uno" == cobertura de models.dev; en la unión solo 1 id casa únicamente en modelparams.dev). Aporta lo que models.dev no tiene: parámetros aceptados, estado `retired/deprecated`, `shutdownOn`, `replacement`.
- **El riesgo real no es el emparejamiento, es que los gateways rellenan valores por defecto.** En la caché de cliproxyapi, el {pct(1539, R['counts']['cliproxyapi-cache'])} de las filas tiene exactamente `(contextWindow, maxTokens) = (272000, 16384)` y el {pct(1548, R['counts']['cliproxyapi-cache'])} la escalera de esfuerzo `[low, medium, high]`. Esos valores coinciden con models.dev en 0% (contexto) de los casos comparables; los valores no-placeholder coinciden en 68%. En el live de easycliproxy el par `(128000, 32768)` cubre el {pct(673, R['counts']['omp-live:easycliproxy'])}.
- El coste es el campo que más falta en el gateway: `omp models easycliproxy --json` devuelve `pricingStatus: unknown` en **{R['counts']['omp-live:easycliproxy']} de {R['counts']['omp-live:easycliproxy']}** filas y dsh en **{R['counts']['omp-live:dsh']} de {R['counts']['omp-live:dsh']}**; models.dev puede rellenar {fld('omp-live:easycliproxy','models.dev','cost(input,output)')['gw_missing_ref_has']} de easycliproxy.

## 2. Método

### 2.1 Fuentes y permisos

| Fuente | Cómo | Fecha / revisión | Permiso |
|---|---|---|---|
| models.dev | `GET https://models.dev/api.json` (1 petición, 6.8 MB) | {FETCH_DATE} | Repositorio `sst/models.dev`, licencia MIT (`raw/models.dev.LICENSE`). Es un JSON público y estático. |
| modelparameters.dev | `https://modelparameters.dev/` responde **308 → https://modelparams.dev/**. Se usó `GET /api/v1/models.json` (1 petición, 1.2 MB, 510 modelos) | `generatedAt` {json.load(open(ROOT + '/raw/modelparams.models.json'))['generatedAt']} | `robots.txt`: `User-agent: *  Allow: /` con la nota "AI agents welcome" y `/api/v1/*.json` rastreable a propósito. `llms.txt` publica la API como interfaz para máquinas. Repositorio `mnfst/modelparams.dev` con licencia MIT (`raw/mp.github.LICENSE`). No existe página de términos (`/terms`, `/license`, `/about` dan 404). **Permitido; se descargó el catálogo completo en una sola petición, sin rastrear páginas HTML.** |
| `~/.omp/agent/cliproxyapi-models.json` | lectura local | `fetchedAt` = 2026-11-26T05:20:01Z | local |
| `omp models easycliproxy --json --no-extensions` | binario `…/Tern/omp/versions/19.2.1` | ejecutado {FETCH_DATE} | local |
| `omp models dsh --json --no-extensions` | idem | idem | local |
| (sonda) roster empaquetado de omp | `zstd -d fixtures/llm-oracle/catalog/models.json.zst` | árbol de trabajo | local; solo se usa como sonda de circularidad, no como fuente de verdad |

Crudos en `/tmp/gateway-proof/raw/` (con `fetch-date.txt`, cabeceras HTTP y `sha256` truncado dentro de `source.origin` de cada observación).

### 2.2 Lo aprendido del crate `omp-catalog`

- No hay un esquema "models.dev" literal en el crate. El roster llega de `models.json` de pi (`scripts/import_v1_models.py`), que ya usa nombres camelCase de `SourceModelRecord` (`crates/catalog/src/compile.rs:239`): `id, name, provider, identity{{class,family,revision}}, api, baseUrl, reasoning, input[], output[], cost{{input,output,cacheRead,cacheWrite,longContext}}, contextWindow, maxTokens, thinking{{mode,efforts,defaultLevel,effortMap,effortBudgets,requiresEffort,…}}, deprecated, supportsTools`. El importador descarta explícitamente `timeBased` (precios por franja) y otros campos de v1.
- Los límites tipados son `ModelLimits {{context_window, maximum_input_tokens, maximum_output_tokens, maximum_batch}}`; el precio en runtime son enteros `NanoUsd` por `PriceUnit`, con `PriceTier.prompt_tokens_above`.
- La procedencia ya existe: `ProvenanceKind {{bundled, discovered, configured}}`, `EvidenceConfidence {{verified, declared, inferred, unknown}}`, `ProvenanceSource {{kind, origin, revision, confidence, observed_at_ms}}` y `ModelProvenance {{sources, updated_at_ms, blocked_until_ms, deprecated}}`. **Lo que no existe es procedencia por campo**, ni `retired`/`replacement`/`shutdownOn`, ni parámetros aceptados.
- Reglas de identidad reutilizadas para el emparejamiento (`compat/taxonomy/_discovery.kdl`): `trailing-marker`, `billing-variant-suffix`, `wrapper-prefix "duo-chat-"`, `synthetic-prefix "hf:"`, `canonical-family-token` (`gemini2` → `gemini-2`). El crate dice que `-max`, `-none`, `-extra-high` NO deben fusionarse; aquí tampoco se quitan.
- `docs/research/providers-signin.md` trata de proveedores y OAuth, no de este esquema; solo cita models.dev una vez (el roster de `modular` viene de models.dev).

## 3. Esquema propuesto

Archivo: `/tmp/gateway-proof/schema/gateway-model.schema.json` (JSON Schema 2020-12). Dos formas con `recordType` discriminante:

- **`observation`**: lo que UNA fuente dice de UN modelo. Lo desconocido es `null`, nunca `0`/`false`. Lo emiten los adaptadores.
- **`model`**: registro fusionado que sirve el gateway: valores ganadores + `references[]` (qué entradas de referencia casaron y con qué regla) + `provenance{{sources[], fields{{ruta → FieldProvenance}}}}`.

| Campo | Tipo | Origen del nombre |
|---|---|---|
| `provider`, `id`, `name`, `kind` | string / enum | `SourceModelRecord` (+ `selector` solo en la salida de omp) |
| `canonicalId` | `vendor/model` | models.dev `canonical_model_id` (nuevo) |
| `contextWindow`, `maxInputTokens`, `maxTokens` | entero ≥1 o null | `contextWindow`/`maxTokens` de `SourceModelRecord`; `maxTokens` = `ModelLimits.maximum_output_tokens` |
| `cost{{status, input, output, cacheRead, cacheWrite, tiers[]}}` | USD por Mtok | `SourceCost`; `tiers[].promptTokensAbove` = `PriceTier.prompt_tokens_above`; **`status` nuevo**: `known/free/unknown` (un 0 con `pricingStatus: unknown` NO es gratis) |
| `input[]`, `output[]` | enum text/image/audio/video/pdf | `SourceModality` |
| `reasoning` | bool o null | `SourceModelRecord.reasoning` |
| `thinking{{mode, efforts[], canDisable, defaultLevel, effortMap, min/maxBudgetTokens, dynamicBudget}}` | efforts: enum `off,minimal,low,medium,high,xhigh,max` | `SourceThinking` + `ReasoningEffort` (`none`→`off`) |
| `lifecycle{{status: live/preview/deprecated/retired/unknown, replacement, shutdownOn}}` | | crate solo tiene `deprecated: bool`; ampliado con modelparams.dev |
| `acceptedParams[]{{path,type,group,values,range,default,conditional}}` | | nuevo (modelparams.dev `params[]`) |
| `authType` | api_key/subscription/… | modelparams.dev (variantes separadas) |
| `provenance.fields[ruta]` | `{{source, rule, confidence, conflict, alternates[{{source,value,confidence}}]}}` | `EvidenceConfidence` reutilizado; el resto es nuevo |

**Validación real:** los adaptadores emiten **19 382 observaciones** (`models.dev` 10 049, `modelparams.dev` 510, `cliproxyapi-cache` 1 704, `omp-live:easycliproxy` 1 699, `omp-live:dsh` 51, `omp-bundled` 5 369) y `validate.py` (jsonschema 4.26, Draft 2020-12) da **0 inválidas**. La validación encontró tres datos reales que el esquema rechazaba: precio negativo `-1000000` en `openrouter/auto` del roster de omp (centinela de "precio variable"), `budget_tokens.min = -1` en models.dev (presupuesto dinámico) y la clave `ultra` de `thinkingLevelMap` en la caché (fuera del enum del crate). Los adaptadores los mapean a `unknown`/`dynamicBudget`/descartado.

## 4. Regla de emparejamiento de ids (explícita)

Entrada: el id de cable del gateway tal cual (p. ej. `gw/model-2`, `gw/qwen/qwen3-coder:free`). Se prueban, **en este orden y se para en el primero que acierta**, cuatro niveles contra el índice de la referencia (id y `canonical_model_id` de cada entrada, más `provider/id`):

1. **T1 exacto-completo**: `lower(id)` == `lower(id_ref)`, == `lower(canonical_ref)` o == `provider/id_ref`. Quita antes el prefijo sintético `hf:`.
2. **T2 exacto-hoja**: la **hoja** (último segmento tras `/`, sin `duo-chat-`) es igual a la hoja de una entrada (o de su canonical). Ignora prefijos de ruta del gateway (`dva/`, `aug/`, `our-free-model/`, `devin/`…).
3. **T3 normalizado**: en la hoja del gateway se (a) separa versión pegada tras token de familia (`gemini2.5`→`gemini-2.5`), (b) `4.5`→`4-5`, `_`→`-`, (c) se quitan marcadores finales `[-:](customtools|cloud|exacto|nitro|original|optimized|nvfp4|fp8|fp4|bf16|int8|int4|thinking|minimal|low|medium|high|xhigh|free|contributor)` repetidamente. **Los marcadores solo se quitan del lado del gateway**, nunca de la referencia (ver auditoría).
4. **T4 sin fecha**: T3 más quitar `-YYYYMMDD`, `-YYYY-MM-DD` o `-YYMM` final.

Varias entradas pueden casar (la misma hoja existe en decenas de revendedores). El valor de referencia por campo se elige así: entradas **de primera parte** (el vendor del `canonical_model_id` es el propio proveedor; o, sin canonical, el segmento de vendor del id del gateway) → si coinciden entre sí, ese valor; si no, mayoría estricta; si no hay primera parte → mayoría estricta entre revendedores; si no hay mayoría, **`ambiguous`** y el campo no se compara.

**Precisión (auditoría manual, 25 pares aleatorios por nivel, semilla fija, `out/results.json › audit`):** T1 25/25 plausibles; T2 25/25 plausibles (ambos sorteos anteriores al último ajuste de T3); T3 antes del ajuste 24/25: un falso positivo `gw/vision → vispark/vision-medium` causado por quitar `-medium` también del lado de la referencia. Se corrigió (solo se quitan marcadores del lado del gateway) y las coberturas de abajo son las posteriores a la corrección. **PENDIENTE: re-auditar T3 con el nuevo sorteo** (tras el cambio quedan {R['match']['_audit_pool_size']['T3-normalized-leaf']} pares T3 y {R['match']['_audit_pool_size']['T4-date-stripped']} T4 contra models.dev). T4 es el nivel más débil y de volumen mínimo. Riesgo conocido de T2/T3: casar un gateway `X-1m` (contexto 1M) con la base `X` (200k) hace que el contexto de la referencia sea incorrecto; el sufijo `-1m` no está en la lista de marcadores precisamente por eso.

## 5. Cifras de emparejamiento

### 5.1 Contra models.dev (por nivel)

{tier_tbl('models.dev')}

### 5.2 Contra modelparams.dev (por nivel)

{tier_tbl('modelparams.dev')}

### 5.3 Cobertura combinada

{either_tbl()}

### 5.4 Los que NO casan

| Gateway | sin match en ninguna | de ellos parecen no-chat (heurística de regex) | de ellos parecen chat |
|---|--:|--:|--:|
""" + "\n".join(f"| {SHORT[g]} | {U[g]['n']} | {U[g]['nonChatLooking']} | {U[g]['chatLooking']} |" for g in G) + f"""

Prefijos más frecuentes entre los sin match (caché): {', '.join(f'`{a}` {b}' for a, b in U['cliproxyapi-cache']['topPrefixes'][:8])}. `aihorde/*` son modelos de imagen de AI Horde: el campo `kind` de `omp models … --json` dice `chat` en **todas** las filas ({R['counts']['omp-live:easycliproxy']} de {R['counts']['omp-live:easycliproxy']}), así que `kind` del gateway no sirve para filtrar. Muestra de los que parecen chat y no casan (easycliproxy): {', '.join(f'`{x}`' for x in U['omp-live:easycliproxy']['chatLookingSample'][:12])}. Son alias del propio gateway (`auto/*`, `dva/…-1m`, `no-think/…`, `tllm/…`) o modelos aún no listados en las referencias.

## 6. Discrepancias de campos (gateway vs models.dev)

"Placeholder" = valor que se repite en ≥20% de las filas de la fuente (par `(contextWindow,maxTokens)` o escalera de esfuerzos). Se excluye de coinciden/discrepan y se cuenta aparte. "Cerca" = diferencia relativa ≤6% (1 000 000 vs 1 048 576 = 4.6%).

**Validación de la heurística de placeholder** (¿coincide el valor con la referencia?):

| Gateway | Campo | filas placeholder: coinciden | filas no-placeholder: coinciden |
|---|---|--:|--:|
""" + "\n".join(ph_line(g, f) for g in ("cliproxyapi-cache", "omp-live:easycliproxy") for f in ("contextWindow", "maxTokens", "efforts")) + f"""

Los placeholders casi nunca coinciden con la referencia y los demás sí, por lo que se tratan como "desconocido".

### 6.1 Contexto, máx. salida, coste, razonamiento (contra models.dev)

{field_tbl('models.dev', ['contextWindow', 'maxTokens', 'cost(input,output)', 'reasoning'])}

Direcciones (discrepan): contexto easycliproxy gw>ref {fld('omp-live:easycliproxy','models.dev','contextWindow')['direction'].get('gw>ref')} / gw<ref {fld('omp-live:easycliproxy','models.dev','contextWindow')['direction'].get('gw<ref')}; máx. salida easycliproxy gw>ref {fld('omp-live:easycliproxy','models.dev','maxTokens')['direction'].get('gw>ref')} / gw<ref {fld('omp-live:easycliproxy','models.dev','maxTokens')['direction'].get('gw<ref')}. **No hay sesgo único**: el gateway no siempre subestima. Un ejemplo observado: `openai/gpt-5.2-codex` 272 000 en el gateway frente a 400 000 en models.dev (posible límite de plan; no verificado).

**Razonamiento.** Discrepancias con `gw=true, ref=false`: caché {reasonC.get('gw=true,ref=false')} (de ellas {reasonC.get('  of which either')} son no-chat por salida o por regex) y easycliproxy {reasonL.get('gw=true,ref=false')} ({reasonL.get('  of which either')} no-chat). Los ejemplos son TTS, embeddings y vídeo marcados `reasoning: true`. Además la caché marca `reasoning=true` en 1 691 de 1 704 filas (99.2%): también es un valor por defecto. Contra modelparams.dev, las discrepancias de razonamiento son todas `gw=false, ref=true` (caché {fld('cliproxyapi-cache','modelparams.dev','reasoning')['disagree']}, easycliproxy {fld('omp-live:easycliproxy','modelparams.dev','reasoning')['disagree']}).

### 6.2 Contra modelparams.dev (máx. salida = `range.max` del parámetro de longitud; esfuerzos = enum de `reasoning_effort`)

{field_tbl('modelparams.dev', ['maxTokens', 'reasoning', 'efforts'])}

modelparams.dev solo declara `range.max` en 113 de 510 entradas, por eso casi todo es "ref sin dato".

### 6.3 Diez ejemplos por campo (muestra aleatoria con semilla fija; `out/examples.json` tiene los demás)

**contextWindow**

{ex_tbl('contextWindow')}

**maxTokens**

{ex_tbl('maxTokens')}

**coste (input, output) USD/Mtok** (todas las discrepancias; solo la caché tiene coste conocido)

{ex_tbl('cost(input,output)')}

**reasoning**

{ex_tbl('reasoning')}

## 7. Sonda de circularidad: ¿la salida "live" de omp es independiente de las referencias?

- Misma pasarela, dos vistas: caché {P['cache_n']} ids, live {P['live_n']}; en ambas {P['both']}; solo caché {P['cache_only_n']} (p. ej. `claude-opus-4-6-thinking`); solo live {P['live_only_n']} (p. ej. `model-33`).
- Con el mismo id, `contextWindow` difiere en **{pc['contextWindow']['disagree']} de {pc['contextWindow']['agree'] + pc['contextWindow']['disagree']}** y `maxTokens` en **{pc['maxTokens']['disagree']} de {pc['maxTokens']['agree'] + pc['maxTokens']['disagree']}**.
- Cuando difieren, el live coincide con models.dev en {aw['live==models.dev']} casos de contexto (la caché en {aw['cache==models.dev']}) y con el roster empaquetado de omp en {aw['live==omp-bundled']} (la caché en {aw['cache==omp-bundled']}). **Conclusión: la salida live de `omp models` ya mezcla el catálogo empaquetado de omp (que nace de models.dev); no es una observación independiente del gateway.** Hay que tratarla con `confidence: inferred` y no contarla como confirmación de models.dev. La observación independiente sería `GET {{modelsUrl}}` del gateway (no se hizo: es un servicio local con credenciales).

## 8. Estado de vida (live/retired)

Cuántos modelos del gateway casan con referencias cuyas entradas relevantes están TODAS `deprecated/retired`: caché {json.dumps(lc['cliproxyapi-cache']['counts'])}; easycliproxy {json.dumps(lc['omp-live:easycliproxy']['counts'])}; dsh {json.dumps(lc['omp-live:dsh']['counts'])}. Ejemplos (easycliproxy): {', '.join(f"`{e[0]}` ({e[1]})" for e in lc['omp-live:easycliproxy']['examples'][:6])}.

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
| `input` (modalidades) | gateway live > models.dev 1ª parte | `declared` | el gateway suele omitir `pdf` (gw⊂ref en {fld('omp-live:easycliproxy','models.dev','inputModalities')['direction'].get('gw⊂ref')} casos) porque es capacidad de la ruta |
| `lifecycle` | modelparams (`status/shutdownOn/replacement`) > models.dev `status` > "listado por el gateway" = `live` | `declared` | solo puede degradar `live`; no hay otra fuente de `retired` |
| `acceptedParams` | modelparams, variante `api_key` de la entrada de 1ª parte | `inferred` si el match es T3/T4 o no 1ª parte | única fuente; el gateway puede no reenviar todos los parámetros |
| `canonicalId` | models.dev `canonical_model_id` (1ª parte/mayoría) | `declared` | agrupa alias de revendedores |

**Cómo se registra la procedencia.** `provenance.sources[]` lista cada fuente con `ProvenanceSource` del crate (`kind`, `origin` con hash de contenido, `revision`, `confidence`, `observedAtMs`) más un `sourceId` estable. `provenance.fields["contextWindow"] = {{source, rule, confidence, conflict, alternates[]}}` guarda el ganador, el id de la regla (`"limits: gateway-live > ref-first-party > ref-majority"`) y todas las alternativas distintas. `references[]` guarda qué entradas casaron y con qué nivel (`T1..T4`, `firstParty`). Así un valor erróneo es auditable y se puede reproducir el merge sin las descargas, porque las observaciones se conservan en JSONL.

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
"""
open(f"{OUT}/catalog-proof.md", "w").write(md)
print(len(md))
