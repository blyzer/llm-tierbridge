# Fuentes del gateway: inventario, licencias y estrategia

Fecha de la investigación: 2026-11-26. Los datos salen de clones locales (`~/repositories/*`), clones superficiales en `/tmp/gw/*`, la API de GitHub (`gh api`) y descargas directas de `models.dev` y `modelparams.dev`. Lo que no pude verificar está marcado como **no verificado**.

> **Actualización 2026-11-26:** la estrategia "Fork + remote upstream" para freellmapi (fila 1, §1 y el
> resumen final) quedó **superada**. freellmapi se usa solo como datos, y su política (ranking, cuotas y
> presupuesto) se reimplementa como plugin de CLIProxyAPI. Ver `design.md` §0.4 y §6.

## Tabla resumen

| # | Fuente | Qué es | Lenguaje | Licencia (SPDX / archivo) | Último commit / release | Valor motor | Valor datos | Estrategia | Riesgo ToS/legal |
|---|---|---|---|---|---|---|---|---|---|
| 1 | **freellmapi** (`tashfeenahmed/freellmapi`) | Router OpenAI-compatible que agrupa tiers gratuitos de ~87 proveedores con claves propias; dashboard, CLI y desktop | TypeScript, Node (server/client/cli/desktop/shared) | MIT / `LICENSE` | 2026-11-25 / v0.16.44 | Alto (fallback chain, bóveda de claves, cuotas, catálogo firmado) | Medio (snapshot en migraciones; el feed vivo es de pago) | ~~Fork + remote upstream~~ → **solo datos**; su política va como plugin de CLIProxyAPI (2026-11-26) | Medio |
| 2 | **CLIProxyAPI** (`router-for-me/CLIProxyAPI`) | Proxy que expone OAuth de CLIs de suscripción (Codex, Claude Code, Antigravity, Grok Build, Devin, Kimi…) como APIs OpenAI/Gemini/Claude | Go 1.26, módulo `…/CLIProxyAPI/v8` | MIT / `LICENSE` | 2026-11-25 / v8.1.4 | Muy alto (traductores, thinking, rotación, plugins) | Alto (`models.json`: 14 canales, 212 modelos, niveles de thinking) | **Sidecar sin modificar + data-only**; fork solo si hay que parchear | **Alto** (pool de cuentas por OAuth) |
| 3a | **Cli-Proxy-API-Management-Center** | WebUI oficial de administración de CLIProxyAPI (`/management.html`) | TypeScript, React 19, Vite 8 | MIT / `LICENSE` | 2026-11-24 / v1.29.2 | Medio (UI) | Bajo | **Usar el asset de release**; vendored base + parches solo si se rebrandea | Bajo (hereda el riesgo de 2) |
| 3b | **EasyCLIProxyAPI** (`router-for-me/EasyCLIProxyAPI`) | Cliente de escritorio de CLIProxyAPI (ciclo de vida del core, OAuth, cuotas, configuración de agentes) | Rust + TypeScript (Tauri) | MIT / `LICENSE` (vía API; no clonado) | 2026-11-25 / v0.3.120 | Bajo para un gateway | Bajo | **No adoptar**; solo referencia | Bajo (hereda el riesgo de 2) |
| 4 | **OmniRoute** (`diegosouzapw/OmniRoute`, ahora `cheaper-inference/OmniRoute`) | Gateway "todo en uno": 287 directorios de proveedor, combos, caché, MITM, cookies web | TypeScript (Next.js + `open-sse`), Node ≥22.22 | MIT / `LICENSE` | 2026-11-26 / v3.8.54 (rama por defecto `release/v3.8.55`) | Alto pero enorme (30 694 archivos) | **Alto** (esquema rico de modelo, free catalog, límites Gemini) | **Data-only / referencia de esquema** | **Alto** (stealth TLS, cookies web, MITM) |
| 5 | **dsh-our-free-model** (`Ebony-Vinyl/dsh-our-free-model`, local) | Plugin para "DeepSeek Harness" (dsh) con carril gratuito de OpenCode, canal Kilo y un paquete de canales de cuentas | JavaScript (ESM, sin build) | MIT + apéndice de terceros / `LICENSE` (GitHub: `NOASSERTION`) | 2026-11-26 / v2.9.1 (el local está en v2.2.0) | Bajo (acoplado al host dsh) | Bajo (`registry.json`: 77 proveedores, 511 modelos, casi todo nulo) | **Data-only; no heredar código** | **Muy alto** |
| 6 | **models.dev** (`sst/models.dev`, ahora `anomalyco/models.dev`) | Base de datos abierta de modelos: specs, precios, límites, capacidades | TypeScript/Bun, datos en TOML | MIT / `LICENSE` | 2026-11-26 / sin releases | Bajo | **Muy alto** (326 proveedores, 10 049 modelos) | **Data-only (consumir `api.json`)** | Bajo |
| 7 | **modelparameters.dev** → canónico **modelparams.dev** (`mnfst/modelparams.dev`) | Catálogo de parámetros de API por modelo (tipo, default, rango, condiciones) con API estática, validador y MCP | TypeScript (+ paquetes npm y PyPI) | MIT / `LICENSE` | 2026-11-24 / `modelparams-py@0.0.53` | Bajo | Alto en parámetros aceptados, pero cobertura pequeña (510 modelos, 22 proveedores) | **Data-only (JSON estático)** | Bajo |

Hallazgo sobre la fuente 7: `modelparameters.dev` sirve el mismo sitio que `modelparams.dev`. Su `robots.txt` apunta a `modelparams.dev`, y `/api` devuelve "API documentation · modelparams.dev".

---

## 1. freellmapi

- **Repo canónico:** https://github.com/tashfeenahmed/freellmapi (58 538 estrellas, creado 2026-04-21). `apollodb/FreeLLMAPI` es un fork en chino, no el original. Sitio y catálogo: https://freellmapi.co.
- **Qué es:** router OpenAI-compatible con cadena de fallback entre tiers gratuitos. Usa claves del usuario y algunos proveedores sin clave (Kilo, OVH, AI Horde). Incluye dashboard, CLI, app de escritorio, API Anthropic/Gemini nativa, emulación de Ollama y media/embeddings/búsqueda.
- **Runtime:** monorepo TypeScript/Node. `server/` (778 archivos fuente y de tests), `client/`, `cli/`, `desktop/`, `shared/`. Persiste en SQLite.
- **Licencia:** MIT, `LICENSE` (Copyright 2026 Tashfeen Ahmed). El router es MIT. El feed de catálogo vivo es un servicio de pago aparte (Premium, 19 USD/año).
- **Actividad:** 1 182 commits desde 2026-04-21. 171 commits desde 2026-10-26 y 54 desde 2026-11-19. 109 tags, último v0.16.44 (2026-11-25).

**Dónde viven modelos y parámetros** (enlaces a `main`):

| Dato | Ruta |
|---|---|
| Registro de modelos (tabla `models`: `platform`, `model_id`, `intelligence_rank`, `speed_rank`, `rpm/rpd/tpm/tpd_limit`, `monthly_token_budget`, `context_window`, `supports_vision`; luego `supports_tools`) | [`server/src/db/migrations/20260101_000000_legacy_baseline.ts`](https://github.com/tashfeenahmed/freellmapi/blob/main/server/src/db/migrations/20260101_000000_legacy_baseline.ts) (el `CREATE TABLE` está en la línea 101) más ~56 migraciones posteriores en [`server/src/db/migrations/`](https://github.com/tashfeenahmed/freellmapi/tree/main/server/src/db/migrations) |
| Costos (equivalente pago, USD por millón de tokens de entrada y salida; fuente OpenRouter, snapshot 2026-06-05) | [`server/src/db/model-pricing.ts`](https://github.com/tashfeenahmed/freellmapi/blob/main/server/src/db/model-pricing.ts) |
| Sincronización de catálogo firmado (Ed25519, 2 veces al día) | [`server/src/services/catalog-sync.ts`](https://github.com/tashfeenahmed/freellmapi/blob/main/server/src/services/catalog-sync.ts) (`DEFAULT_BASE_URL = https://api.freellmapi.co`) |
| Quirks por modelo y plataforma (`migrateQuirksV1`) | misma migración baseline, desde la línea ~2100 |
| Adaptadores de proveedor (69 archivos) | [`server/src/providers/`](https://github.com/tashfeenahmed/freellmapi/tree/main/server/src/providers) |
| Unión `Platform` | [`shared/types.ts`](https://github.com/tashfeenahmed/freellmapi/blob/main/shared/types.ts) |
| Documentación de plataformas (keyed/keyless, límites observados) | [`docs/en/providers/01-supported-platforms.md`](https://github.com/tashfeenahmed/freellmapi/blob/main/docs/en/providers/01-supported-platforms.md) |

- **Niveles de razonamiento y parámetros aceptados:** no hay campos de primera clase para ellos. Solo existen `supports_tools`, `supports_vision` y los quirks. Para reasoning/params hay que usar fuentes 2, 4, 6 o 7.
- **Valor motor:** alto. Es el motor más cercano a un "gateway de tiers gratuitos": fallback chain, cuotas por modelo, bóveda cifrada y estrategias `auto:*`.
- **Valor datos:** medio. Lo bundleado es un baseline más migraciones. El README dice que los installs gratuitos reciben el snapshot mensual, con unos 30 días de retraso (unos 303 modelos detrás), y que el feed vivo requiere una licencia `fla_`.
- **Estrategia: fork + remote upstream.** Motivo: el registro vive en migraciones TS aditivas (56 archivos), así que los parches propios son migraciones nuevas y casi no chocan con 54 commits por semana.
- **Riesgos:**
  - **Feed vivo (Premium):** no raspar `api.freellmapi.co` ni redistribuir ese catálogo; sus términos **no fueron verificados**.
  - **ToS de proveedores:** el propio repo revisó los ToS en mayo 2026 ([`docs/en/architecture/00-high-level-index.md#terms-of-service-review`](https://github.com/tashfeenahmed/freellmapi/blob/main/docs/en/architecture/00-high-level-index.md#terms-of-service-review)). Marca Cohere ❌ (los términos prohíben uso personal), y Gemini, NVIDIA NIM, Z.ai y Cloudflare ⚠️.
  - **Reglas del propio proyecto:** una cuenta por proveedor, sin reventa, sin compartir el endpoint. Su README declara que el proyecto es "para experimentación personal, no producción".
  - **Canales sin clave:** Kilo registra los prompts para entrenamiento. OVH permite 2 req/min por IP, y esos límites son por IP, no por cuenta.

## 2. CLIProxyAPI

- **Repo:** https://github.com/router-for-me/CLIProxyAPI (59 508 estrellas, creado 2025-07-01). Clon local en `~/repositories/CLIProxyAPI`, limpio en `main` en v8.1.4.
- **Qué es:** servidor proxy que expone OpenAI (incl. Responses), Gemini, Claude y Codex a partir de **cuentas de CLIs con OAuth** (varias cuentas con balanceo). Canales de `models.json`: `claude`, `codex-free/team/plus/pro`, `gemini`, `gemini-cli`, `vertex`, `aistudio`, `antigravity`, `kimi`, `xai`, `copilot`, `meta`. Además tiene un catálogo Devin y soporte de plugins.
- **Runtime:** Go 1.26.10. El módulo es `github.com/router-for-me/CLIProxyAPI/v8`. La lógica está en `internal/` (no importable desde otros módulos). La superficie pública embebible es `sdk/` (`cliproxy`, `translator`, `access`, `auth`, `pluginabi`…).
- **Licencia:** MIT, `LICENSE` (Copyright 2025 Luis Pater / 2025-presente Router-For.ME).
- **Actividad:** 5 201 commits en total y 658 desde 2026-10-26. Releases casi diarios (v8.1.2 → v8.1.4).

**Dónde viven modelos y parámetros:**

| Dato | Ruta |
|---|---|
| Catálogo general embebido (160 KB; 14 canales, 212 modelos) | [`internal/registry/models/models.json`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/registry/models/models.json) |
| Catálogos Codex (1 MB) y Devin | [`codex_client_models.json`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/registry/models/codex_client_models.json), [`devin_models.json`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/registry/models/devin_models.json) |
| Refresco remoto (`ModelsRefreshInterval = 3h`; URLs: raw de `router-for-me/models` y `https://models.router-for.me/models.json`) | [`internal/registry/model_updater.go`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/registry/model_updater.go), [`catalog_sources.go`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/registry/catalog_sources.go) |
| Registro dinámico (cuotas, suspensión) | [`internal/registry/model_registry.go`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/registry/model_registry.go) |
| Lógica de thinking/effort por proveedor | [`internal/thinking/`](https://github.com/router-for-me/CLIProxyAPI/tree/main/internal/thinking) |
| Exclusiones y alias | [`internal/modelconfig/`](https://github.com/router-for-me/CLIProxyAPI/tree/main/internal/modelconfig) |
| API de gestión relacionada | [`internal/api/server_management.go`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/api/server_management.go): `/model-definitions/:channel`, `/auth-files/models`, `/oauth-excluded-models`, `/oauth-model-alias`. [`server_management_v8.go`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/api/server_management_v8.go): `/routing/model-definitions/refresh`, `/models/refresh`, `/credentials/models`. Doc: [`docs/management-api-v8.md`](https://github.com/router-for-me/CLIProxyAPI/blob/main/docs/management-api-v8.md) |

**Cobertura de campos en `models.json`:** 212 modelos; los porcentajes son sobre ese total.

| Campo | Cobertura |
|---|---|
| `context_length`, `max_completion_tokens` | 153 (72%) |
| `inputTokenLimit`, `outputTokenLimit` | 77 |
| `thinking` | 193; de ellos 164 con `levels` (p. ej. `low/medium/high/xhigh/max`) y 72–82 con `min`/`max` de presupuesto |
| `supported_parameters` | 41 |
| `supportedInputModalities` | 210 |
| `supports_web_search` | 10 |
| `native_capabilities` | 117 |
| Costos | **ninguno** |

- **Feed de datos separado:** [`router-for-me/models`](https://github.com/router-for-me/models) (24 commits desde 2026-10-26, 175 desde 2026-03-10). Contiene los mismos JSON más `native-capabilities-evidence.json` y `gemini-native-search-declaration.json`. **No tiene archivo `LICENSE`** (GitHub: sin licencia). Los mismos archivos dentro del repo CLIProxyAPI sí caen bajo su MIT. Para reutilizar datos, extraerlos del repo principal, no del feed.
- **Valor motor:** muy alto. Traductores entre protocolos, thinking, rotación y refresco de OAuth, plugins.
- **Valor datos:** alto en niveles de thinking y límites. Sin costos, y centrado en canales de suscripción.
- **Estrategia: sidecar sin modificar (binario o Docker fijado, usado vía API de gestión) + data-only.** Fork + remote upstream solo si hace falta un parche. Motivo: 658 commits en 30 días y paquetes `internal/` no importables hacen que un fork profundo sea caro de mantener. Con un fork solo conviene llevar un patch stack mínimo, sobre el path de módulo `/v8`.
- **Riesgos:**
  - El valor central es reenviar tráfico por **suscripciones OAuth de terceros** (Claude Code, Codex, Antigravity, Grok, Kimi, Devin) y rotarlas entre varias cuentas. Es el mismo patrón de "pool de cuentas".
  - No consulté los ToS de Anthropic, OpenAI ni Google en esta corrida (**no verificado**). Recomiendo tratarlo como incompatible con ToS de consumo hasta confirmar lo contrario.
  - Para un producto, limitarse a los canales de clave API (`gemini`, `vertex`, `aistudio`, compatibilidad OpenAI) y desactivar los de OAuth.

## 3. Interfaces de administración de CLIProxyAPI

### 3a. Cli-Proxy-API-Management-Center

- **Repo:** https://github.com/router-for-me/Cli-Proxy-API-Management-Center (4 843 estrellas). Clon local en `~/repositories/Cli-Proxy-API-Management-Center`.
- **Qué es:** WebUI oficial. Compila a un único `dist/index.html` (plugin `vite-plugin-singlefile`) que el release publica como `management.html`. El backend lo descarga de `releases/latest` ([`internal/managementasset/updater.go`](https://github.com/router-for-me/CLIProxyAPI/blob/main/internal/managementasset/updater.go)) y lo sirve en `http://<host>:<puerto>/management.html`.
- **Stack:** TypeScript, React 19.3, Vite 8, TS 6, zustand 5, axios, i18next. Bun.
- **Licencia:** MIT, `LICENSE` (Copyright 2026 Router-For.ME).
- **Actividad:** v1.29.2 (2026-11-24). 1 428 commits en total, 124 desde 2026-10-26.
- **Dónde viven modelos y parámetros:** la UI no guarda datos. Llama a la API de gestión desde [`src/services/api/models.ts`](https://github.com/router-for-me/Cli-Proxy-API-Management-Center/blob/main/src/services/api/models.ts), `providerModels.ts`, `routing.ts` y `authFiles.ts`. Hay módulos de cuota por proveedor (`kimiQuota.ts`, `devinQuota.ts`, `claudeUsage.ts`, `metaQuota.ts`) y features `authFiles`, `config`, `dashboard`, `logs`, `oauth`, `plugins`, `providers`, `quota`, `routing`.
- **Estrategia: usar el asset de release sin forkear.** Si se necesita rebranding o vistas propias: vendored base + patch stack (124 commits en 30 días sobre una SPA, parches de UI acotados).
- **Riesgo:** bajo propio. Expone y facilita la gestión de las cuentas OAuth de la fuente 2.

### 3b. EasyCLIProxyAPI

- **Repo:** https://github.com/router-for-me/EasyCLIProxyAPI (3 786 estrellas, creado 2025-09-07). Es el "EasyCLIProxyAPI" oficial. No lo clonamos: metadatos y README vía `gh api`.
- **Qué es:** consola de escritorio portátil (Tauri). Gestiona ciclo de vida del core, OAuth, agregación de proveedores, cuotas, alias de modelos y configuración automática de agentes. Puede llevar el core embebido (`core-version.txt` = 8.1.3, una versión por detrás de v8.1.4).
- **Lenguajes (bytes):** Rust 5.6 M, TypeScript 5.4 M, JS 1.2 M, CSS 0.8 M, Go 89 K.
- **Licencia:** MIT por API de GitHub. El archivo `LICENSE` existe en la raíz. No leí su texto.
- **Actividad:** v0.3.120 (2026-11-25); v0.3.110 salió el 2026-11-17, o sea unas 10 releases en ~8 días. Número total de commits no medido.
- **Estrategia: no adoptar.** Motivo: es cliente de escritorio de un solo usuario y no aporta motor ni datos a un gateway.
- **Riesgo:** hereda el de la fuente 2.

## 4. OmniRoute

- **Repo:** https://github.com/cheaper-inference/OmniRoute (la API de GitHub redirige `diegosouzapw/OmniRoute` a esta ruta). 90 403 estrellas, creado 2026-02-13. Clon superficial en `/tmp/gw/omniroute`.
- **Qué es:** gateway que se anuncia con 375 proveedores (150+ gratuitos) y 1 400+ modelos, con combos, auto-combo, caché semántica, guardrails y proveedores por cookie web. Tiene 287 directorios de proveedor en `open-sse/config/providers/registry/` (conteo propio); el 375 es cifra del README, no recontada.
- **Runtime:** TypeScript (Next.js + `open-sse`), Node ≥22.22.2, 30 694 archivos (10 718 tests, 10 556 docs).
- **Licencia:** MIT, `LICENSE` (Copyright 2026 Cheaper Inference – Diego Souza).
- **Actividad:** v3.8.54 (2026-11-17). Releases cada 10–20 días (3.8.50 del 08-26, 3.8.51 del 09-30, 3.8.52 del 10-19, 3.8.53 del 11-06). **La rama por defecto es `release/v3.8.55`**, no `main`. Según el conteo por paginación de la API, unos **4 270 commits en 30 días**.

**Dónde viven modelos y parámetros:**

| Dato | Ruta |
|---|---|
| Esquema `RegistryModel`: `toolCalling`, `supportsReasoning`, `alwaysReasons`, `supportedThinkingEfforts`, `supportsXHighEffort`, `supportsVision/Audio/Video`, `contextLength`, `maxInputTokens`, `maxOutputTokens`, `unsupportedParams`, `strip`, `targetFormat` | [`open-sse/config/providers/shared.ts`](https://github.com/cheaper-inference/OmniRoute/blob/release/v3.8.55/open-sse/config/providers/shared.ts) |
| Registro por proveedor | `open-sse/config/providers/registry/<proveedor>/index.ts`, reexportado por [`open-sse/config/providerRegistry.ts`](https://github.com/cheaper-inference/OmniRoute/blob/release/v3.8.55/open-sse/config/providerRegistry.ts) |
| Catálogos de modelos gratuitos y retirados | `open-sse/config/freeModelCatalog.data.ts` (579 líneas), `freeModelCatalog.retired.data.ts`, `freeTierCatalog.ts`, `freeTierProviders.ts` |
| Límites de tasa Gemini | `open-sse/config/geminiRateLimits.json` |
| Niveles de razonamiento | `open-sse/config/reasoningEfforts.ts`, `open-sse/executors/base/reasoningEffort.ts` |
| Registros por modalidad | `audioRegistry.ts`, `embeddingRegistry.ts`, `imageRegistry*.ts`, `rerankRegistry.ts`, `videoRegistry.ts`… en `open-sse/config/` |
| Costos | `src/shared/constants/pricing.ts`, `pricing/default-pricing.ts`, `src/lib/pricingSync.ts` (sincroniza con LiteLLM; opt-in con `PRICING_SYNC_ENABLED`, por defecto apagado), `src/lib/usage/modelPricingRegistry.ts` |

- **Relación con models.dev:** el contrato del carril gratuito de OpenCode usa `models.dev` para decidir si un id `-free` está disponible ([`open-sse/executors/opencodeFreeTierContract.ts`](https://github.com/cheaper-inference/OmniRoute/blob/release/v3.8.55/open-sse/executors/opencodeFreeTierContract.ts)).
- **Valor motor:** alto pero demasiado grande. Es un producto completo con dashboard, MITM, Electron, Docker y CLI.
- **Valor datos:** el más rico en parámetros por modelo (`unsupportedParams`, `strip`, `supportedThinkingEfforts`). Está disperso en 320 archivos TS por proveedor.
- **Estrategia: data-only / referencia de esquema.** Motivo: con ~4 270 commits/mes y 30 694 archivos un fork no se puede mantener. MIT permite reutilizar el esquema y los datos con atribución.
- **Riesgos (altos):**
  - [`docs/security/STEALTH_GUIDE.md`](https://github.com/cheaper-inference/OmniRoute/blob/release/v3.8.55/docs/security/STEALTH_GUIDE.md) admite que integra proveedores cuyos bordes "fingerprint non-official clients" (TLS JA3/JA4, orden de headers, forma del body, tokens de integridad). Incluye `claudeCodeFingerprint`, `claudeCodeObfuscation` y `src/mitm/`.
  - Documenta proveedores por **cookie de navegador** ([`docs/getting-started/WEB-COOKIE-GUIDE.md`](https://github.com/cheaper-inference/OmniRoute/blob/release/v3.8.55/docs/getting-started/WEB-COOKIE-GUIDE.md)), una extensión Chrome de sincronización de sesiones y detección de baneos (`docs/security/BAN_DETECTION.md`).
  - Su guía dice que esas funciones son para cuentas propias oficiales y "no para violar ToS", y que el operador debe cumplir los ToS. Eso traslada la responsabilidad al usuario y no la elimina.
  - No copiar `claude*Fingerprint*`, `tlsClient*`, `src/mitm/` ni los providers web-cookie.

## 5. dsh-our-free-model (repo local)

- **Repo:** local `~/repositories/dsh-our-free-model`, origin `git@github.com:Ebony-Vinyl/dsh-our-free-model.git`. 87 559 estrellas, creado 2026-09-24.
- **Estado del clon local:** **desactualizado**. Su `main` está en v2.2.0 (2026-10-14, 428 commits). En esta corrida ejecuté `git fetch` en ese repo, que solo actualizó refs remotas; el árbol de trabajo y la rama local no se tocaron. Tras el fetch, `origin/main` está en `b1142bd` (v2.9.1, 2026-11-26) y aparecen ramas nuevas (`rewrite/independent-core`, `v2.3beta`). El análisis de abajo usa un clon superficial de `origin/main` en `/tmp/gw/dsh`.
- **Qué es:** plugin para "DeepSeek Harness" (dsh) que monta un carril de modelos gratuitos:
  - **Carril gratuito:** pasarela de OpenCode `https://opencode.ai/inference/v1/*`, con `Authorization: Bearer public`.
  - **Canal Kilo:** `https://api.kilo.ai/api/gateway`, sin cuenta.
  - **Pestaña Aggregate:** 77 proveedores por clave propia.
  - **Canal EAC:** pasarela alojada por los mantenedores, que exige autorizar GitHub y dar una estrella al repo.
  - **`vendor/channel-pack`:** paquete de canales de cuentas.
- **Runtime:** JavaScript ESM, sin build, "zero dependencies". Núcleo en `src/` (`index.js` 110 KB, `client.js` 442 KB).
- **Licencia:** MIT (badge del README y encabezado de `LICENSE`). En v2.2.0 el archivo medía 1 KB. En `origin/main` mide ~14 KB: MIT más apéndice de avisos de terceros (models.dev MIT, opencode MIT, al menos). Por eso GitHub reporta `NOASSERTION`. El texto de los otros apéndices no lo leí completo.
- **Actividad:** v2.9.1 (2026-11-26) y último commit 2026-11-26T07:34Z.

**Dónde viven modelos y parámetros:**

| Dato | Ruta |
|---|---|
| Lista del carril gratuito | no se guarda. `GET opencode.ai/inference/v1/models` se refresca ([`src/upstream.js`](https://github.com/Ebony-Vinyl/dsh-our-free-model/blob/main/src/upstream.js), [`src/catalog.js`](https://github.com/Ebony-Vinyl/dsh-our-free-model/blob/main/src/catalog.js)) |
| Lista Kilo | `GET api.kilo.ai/api/gateway/models` ([`src/kilo.js`](https://github.com/Ebony-Vinyl/dsh-our-free-model/blob/main/src/kilo.js)) |
| Registro Aggregate: 77 proveedores, 511 modelos. Cada modelo solo trae `id`, `name`, `contextWindow`, `vision`, `reasoning`, y muchos valores son `null`. Por categoría: 61 apikey, 10 free-tier, 4 local, 2 oauth. Por origen: 30 `models.dev`, 20 `official`, 15 `9router`, 12 `custom` | [`src/providers/registry.json`](https://github.com/Ebony-Vinyl/dsh-our-free-model/blob/main/src/providers/registry.json) |
| Esfuerzo de razonamiento: presupuestos de `max_tokens`, no etiquetas. El gateway ignora `reasoning_effort` y otros campos | [`src/effort.js`](https://github.com/Ebony-Vinyl/dsh-our-free-model/blob/main/src/effort.js) |
| Costos y límites por proveedor | ninguno propio. Sigue la convención `limit.context`/`limit.output` de models.dev y 9router |

- **Valor motor:** bajo. Es un plugin acoplado a las APIs de host de dsh (`@deepseek-ai/*`), con UI, actualizador y firmas propios.
- **Valor datos:** bajo. Su registro es derivado de models.dev, 9router y free-claude-code (AGPL-3.0-only, citado como referencia). Es más pobre que la fuente original.
- **Estrategia: data-only. No forkear ni vendorizar código.** Motivo: lo único diferencial es la suplantación de clientes y los pools de cuentas, que no conviene heredar; el resto se obtiene de models.dev. Pendiente de decisión: `rewrite/independent-core` (rama nueva, no inspeccionada).
- **Riesgos (muy altos):**
  - **Suplantación del cliente de OpenCode.** [`LICENSE`](https://github.com/Ebony-Vinyl/dsh-our-free-model/blob/main/LICENSE) dice que el carril "debe reproducir el cuerpo de la petición del cliente oficial OpenCode byte a byte, porque el tier gratuito lo fingerprintea". `src/upstream.js` importa `mintSessionIdentifier`, `injectOfficialPrompt`, `isOfficialSessionID`, `randomProjectID` y `CLIENT_UA`. Su README admite que el gateway fingerprintea por `x-opencode-*`, cuenta uso por sesión y devuelve 403 por región y 429 al agotar. Eso es esquivar un control de acceso del proveedor.
  - **Pools de cuentas.** `vendor/channel-pack` ([`NOTICE.md`](https://github.com/Ebony-Vinyl/dsh-our-free-model/blob/main/vendor/channel-pack/NOTICE.md)) aporta "login, pool de cuentas, reclamo de créditos, lista negra de modelos y gateway OpenAI local" para diez canales (LobsterAI, Qoder, Qoder CN, TRAE, Cline, Loomy, Raccoon, MiniMax Code, ZCode, Gemini). Hay archivos `captcha-carrier.ts`, `captcha-backoff.ts`, `qoder-machine.ts` (huella de máquina), `loomy-sign.ts` y un `qoder-auth-wasm.wasm` binario sin fuente. El código viene de Jet (gitee.com/iJetLi, MIT, commit `345f0a0…`). En `docs/pr97-reviewed-channels.md` se habla de "13 canales de cuentas" y de un RPC de creación de cuentas por navegador para OpenCode (desactivado en el montaje de OFM). Es farmeo de cuentas gratuitas con resolución de captcha; riesgo de ToS y posiblemente legal.
  - **Contradicción documental.** El README dice "No account pool, no relay, no reseller" para las dos fuentes sin clave, pero `channel-pack` sí monta pools de cuentas. Lo leo como que el texto solo cubre los dos carriles anónimos.
  - **Binarios opacos** (`vendor/magpie/bin/magpie-win32-x64.exe`, `.wasm`): riesgo de cadena de suministro.
  - **Privacidad:** Kilo avisa en su ficha que los prompts pueden registrarse y usarse para mejorar sus servicios.
  - ToS de OpenCode Zen y Kilo: **no verificados**.

## 6. models.dev

- **Repo canónico:** https://github.com/anomalyco/models.dev. `sst/models.dev` redirige a esa ruta (7 760 estrellas, creado 2025-06-04). Clon superficial en `/tmp/gw/modelsdev`. Sitio: https://models.dev.
- **Qué es:** base de datos comunitaria y abierta de specs, precios y capacidades. La usa OpenCode internamente.
- **Runtime:** TypeScript/Bun. Datos en TOML. Desplegado con SST en Cloudflare.
- **Licencia:** MIT, `LICENSE` (Copyright 2025 models.dev). No hay licencia de datos aparte.
- **Actividad:** último commit 2026-11-26 y sin releases. Rama por defecto `dev`.
  - Commits: 465 en 7 días, 2 482 en 30 días y 7 613 en 90 días (conteo de commits de GitHub, la mayoría automáticos).
  - Cadencia: [`sync-models.yml`](https://github.com/anomalyco/models.dev/blob/dev/.github/workflows/sync-models.yml) corre con cron `17 */6 * * *` (cada 6 h), abre PRs por proveedor y los auto-fusiona si son seguros. Más contribuciones manuales.

**API y dumps** (verificados con `curl` el 2026-11-26):

| URL | Contenido |
|---|---|
| https://models.dev/api.json | Catálogo v1 completo: 6,8 MB, **326 proveedores, 10 049 modelos**. Cabeceras: `access-control-allow-origin: *`, `etag`, `cache-control: public, max-age=0, must-revalidate` (usar ETag) |
| `https://models.dev/api.json?type=decision` / `?type=all` | Filtro por tipo de modelo (por defecto se omiten los tipos especializados) |
| https://models.dev/models.json | Metadatos del modelo independientes del host |
| https://models.dev/catalog.json | Proveedores y modelos juntos |
| `https://models.dev/experimental/v2.0/api.json?include=openai-codex` | v2 experimental. Los proveedores OAuth (`openai-codex`, `openai-chatgpt`) son opt-in y la v1 nunca los expone |
| `https://models.dev/logos/{provider}.svg` | Logos |

**Esquema:**
- **Archivos fuente (v2):** `models/<lab>/<modelo>.toml` (hechos del modelo: modalidades, `limit`, `capabilities`) y `providers/<prov>/models/<modelo>.toml`. El segundo usa `base_model = "<lab>/<modelo>"` para heredar y define `capabilities.reasoning = { supported, effort = [...] }`, `[cost.text.input]`/`output` con `overrides` por contexto, y `[limit]`. Esquema en [`packages/core/src/schema-v2.ts`](https://github.com/anomalyco/models.dev/blob/dev/packages/core/src/schema-v2.ts); reglas en `AGENTS.md`.
- **Campos de `api.json` v1** (cobertura sobre 10 049 modelos):

| Campo | Cobertura |
|---|---|
| `id`, `name`, `description`, `attachment`, `reasoning`, `tool_call`, `release_date`, `modalities`, `limit` (`context`, `output`) | 100 % |
| `temperature` | 9 731 |
| `cost` (`input`, `output`, `cache_read`, `cache_write`, `tiers`, `context_over_200k`, `reasoning`, audio) | 9 599 |
| `reasoning_options` (p. ej. `[{"type":"effort","values":["low","high"]}]`) | 8 056 |
| `structured_output` | 7 554 |
| `canonical_model_id` | 8 772 |
| `limit.input` | 1 987 |
| `interleaved`, `status`, `experimental` | parcial |

- **Valor motor:** ninguno relevante. **Valor datos:** el mayor de todas las fuentes en costos, límites, niveles de razonamiento y modalidades.
- **Estrategia: data-only.** Consumir `api.json` con ETag y fijar un snapshot en el repo. Motivo: licencia MIT, API pública con CORS abierto y 4 refrescos al día, todo sin tocar código ajeno.
- **Riesgos:** bajos. Se debe incluir el aviso MIT. No cubre cuotas de tiers gratuitos ni límites de tasa por cuenta. La calidad de los campos `reasoning_options` en relays es heurística, según su propio README.

## 7. modelparameters.dev (canónico: modelparams.dev)

- **Repo:** https://github.com/mnfst/modelparams.dev (23 estrellas, creado 2026-05-20). Sitio https://modelparams.dev. `modelparameters.dev` sirve el mismo contenido.
- **Qué publica:** el catálogo de **parámetros de API** de cada modelo (`temperature`, `top_p`, `max_tokens`…), con tipo, default, rango y condiciones. No publica pesos ni parámetros del modelo. Los datos viven en `models/<proveedor>/…` (22 proveedores) y `providers.yaml`.
- **Runtime y paquetes:** TypeScript. Paquete npm `modelparams` y paquete PyPI `modelparams` (release `modelparams-py@0.0.53`). Hay servidor MCP y un Worker de Cloudflare.
- **Licencia:** MIT, `LICENSE` (del repo). No encontré una licencia de datos aparte (**no verificado**).
- **Actividad:** último commit 2026-11-24. Solo 5 commits desde 2026-10-27, así que el ritmo es lento.

**API (solo páginas estáticas y JSON, más un validador):**

| URL | Contenido |
|---|---|
| https://modelparams.dev/api/v1/models.json | Índice: `$schema`, `generatedAt`, `count` = **510 modelos** |
| `https://modelparams.dev/api/v1/models/<proveedor>/<modelo>.json` | Ficha por modelo: `provider`, `authType`, `apiSurface` (p. ej. `openai-chat-completions`), `model`, `status` y `params[]` con `path`, `label`, tipo, default, rango y condiciones de gating |
| https://modelparams.dev/api/v1/schema.json | JSON Schema (convención en `docs/model-parameters-schema.md`) |
| `POST /api/v1/validate` | Valida un payload y devuelve una versión corregida |
| `/llms.txt`, `/llms-full.txt`, `/sitemap.xml`, `/glossary` | Descubrimiento para agentes; el glosario define 80 parámetros |

- **Términos y robots:** `robots.txt` permite todo (`User-agent: *`, `Allow: /`, "AI agents welcome") y dice que `/api/v1/*.json` es rastreable a propósito. No existe una página de términos (`/terms` devuelve 404). Lo único formal es la licencia MIT del repo. El README avisa que las peticiones que llegan al Worker (MCP y `validate`) se registran (endpoint, modelo, modo de auth); los JSON estáticos no.
- **Valor motor:** ninguno. **Valor datos:** alto para "qué parámetros acepta cada modelo y cuáles combinaciones rechaza el proveedor". Es el único dato estructurado de esa categoría entre las fuentes, y complementa a models.dev (que casi no modela parámetros).
- **Estrategia: data-only.** Descargar el JSON estático en CI y fijar un snapshot. No enviar payloads reales a `/validate`; validar en local con el paquete npm/PyPI. Motivo: MIT y JSON estático sin cuenta ni cuota.
- **Riesgos:** bajos. Cobertura chica (510 modelos, 22 proveedores) y proyecto joven con pocos mantenedores, por lo que debe cruzarse con models.dev.

---

## Resumen de estrategias y por qué

1. **freellmapi:** fork + remote upstream. Registro basado en migraciones aditivas y MIT.
2. **CLIProxyAPI:** sidecar sin modificar + data-only. 658 commits/mes y `internal/` no importable.
3. **Management Center / EasyCLIProxyAPI:** asset de release para el primero y no adoptar el segundo. Son UI de la fuente 2.
4. **OmniRoute:** data-only / referencia de esquema. ~4 270 commits/mes, 30 694 archivos y módulos de stealth/cookies.
5. **dsh-our-free-model:** data-only; no heredar código. Suplantación de OpenCode (documentada en su `LICENSE`) y pools de cuentas con captcha.
6. **models.dev:** data-only. MIT, `api.json` público, refresco cada 6 h.
7. **modelparams.dev:** data-only. MIT, JSON estático, único dato de parámetros aceptados.

## Matriz de dónde conseguir cada dato

| Dato | Mejor fuente | Complemento |
|---|---|---|
| Registro de modelos y proveedores | models.dev (`api.json`) | OmniRoute (esquema), freellmapi (cobertura de tiers gratuitos) |
| Límites (contexto, salida) | models.dev | CLIProxyAPI `models.json` |
| Costos | models.dev (`cost`) | freellmapi `model-pricing.ts`, OmniRoute `pricing/` |
| Niveles de razonamiento | models.dev (`reasoning_options`) | CLIProxyAPI `thinking.levels`, OmniRoute `supportedThinkingEfforts` |
| Parámetros aceptados | modelparams.dev | OmniRoute `unsupportedParams`/`strip`, CLIProxyAPI `supported_parameters` |
| Cuotas de tier gratuito (rpm/rpd/tpm/tpd) | freellmapi (baseline) | OmniRoute `geminiRateLimits.json`, `freeModelCatalog.data.ts` |

## Lo que no verifiqué

- ToS de Anthropic, OpenAI, Google, OpenCode Zen y Kilo.
- Términos del feed Premium de freellmapi y del sitio modelparams.dev (no hay `/terms`).
- Texto completo del apéndice de terceros en la `LICENSE` de dsh, y cualquier licencia de datos separada en modelparams.dev.
- Cifras de marketing de OmniRoute (375 proveedores, 1 400+ modelos) y de freellmapi (87 proveedores, 1 313 endpoints); no las recontré.
- Código fuente de EasyCLIProxyAPI (solo metadatos y README por API).

## Procedencia y efectos secundarios

- Clones superficiales en `/tmp/gw/{omniroute,modelsdev,dsh}`. Descargas en `/tmp/gw/api.json` y `/tmp/gw/mdh.txt`.
- `git fetch` en `~/repositories/dsh-our-free-model`: solo refs remotas. Rama local y árbol de trabajo sin cambios.
- Los repos locales de freellmapi, CLIProxyAPI y Cli-Proxy-API-Management-Center no se modificaron.
