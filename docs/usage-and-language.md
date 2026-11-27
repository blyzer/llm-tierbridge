# Carga real del gateway y elección de lenguaje (Rust vs Go vs Elixir/BEAM)

Medido el 2026-11-26 (hora local -04:00). Ventana: últimos 7 días (desde 2026-11-19 ~04:30 local). Todo número sale de una consulta (jq / sqlite3 / Python) sobre archivos locales; nada estimado.

## 1. Fuentes y su cobertura (qué se pudo y no se pudo medir)

| Fuente | Qué contiene | Uso |
|---|---|---|
| `~/.omp/logs/omp.*.log` (17 006 archivos, 368 MB, 2026-11-20 → 11-26; ~1,4 M líneas) | Solo ruido operativo: 1 140 207 avisos `plugins: stale lockfile`, 126 035 `Usage fetch queued`, etc. **No hay un registro por request.** Solo 729 `agent_end`, 190 `Input reached its model request`, 236 `provider proxy resolved`, 141 `advisor turn failed` (parcial: debug solo activo desde ~11-23). | Evidencia complementaria (errores, proveedores resueltos) |
| `~/.omp/agent/sessions/**/*.jsonl` (9 100 archivos; 174 con actividad en 7 d) | Cada mensaje `assistant` con `provider`, `model`, `timestamp` (inicio ms), `completedAt` (fin ms), `stopReason`, `errorMessage`; entradas `model_change` con `reason: fallback`. | **Fuente principal de requests y concurrencia de omp** |
| `~/.omniroute/storage.sqlite` → `call_logs` (4 047 filas, 3 696 en 7 d) | método, path, status, provider, duración, combo, `combo_execution_key`, `queue_wait_ms` | Tráfico y concurrencia de OmniRoute (puerto 20128) |
| `~/.cli-proxy-api/usage-report.sqlite` → `usage_events` (18 415 filas, 2 749 en 7 d) | provider, executor, model, `latency_ms`, tokens, `failed` | Tráfico y concurrencia de CLIProxyAPI (puerto 8317) |
| `~/Library/Application Support/com.cpa.gui/cpa-core/` | `cli-proxy-api` 70 MB (Go, `go 1.26.10`), sin directorio `logs/` | Config/binario (EasyCLIProxyAPI.app lo envuelve) |

Limitación honesta: el tiempo de inicio vs fin de `call_logs`/`usage_events` no está documentado; calculé ambos supuestos (ts=inicio y ts=fin) y reporto el rango.

## 2. Requests por proveedor (7 días)

**omp → proveedor (mensajes assistant en sesiones; 11 526 requests, 174 archivos de sesión)**

| Proveedor (en omp) | Requests |
|---|---|
| anthropic | 5 890 |
| cliproxyapi | 2 373 |
| xai-oauth | 1 550 |
| openai-codex | 1 087 |
| claude-bridge | 291 |
| omni (OmniRoute) | 204 |
| easycliproxy | 45 |
| grok / devin / omniroute / google / zai / cursor / kilo-free / dsh | 33 / 24 / 12 / 6 / 3 / 3 / 3 / 2 |

Tráfico que pasa por un gateway local (cliproxyapi + easycliproxy + omni + omniroute + dsh) = **2 636 de 11 526 (22,9 %)**. Por día: 988 / 1 492 / 1 558 / 847 / 1 738 / 2 403 / 2 500 (11-19 … 11-26; sin datos el 11-22).

**Dentro de OmniRoute (call_logs, 3 696 filas; 2 978 son `/v1/chat/completions`, 641 son `/api/providers/test`, 77 sin path)**: auggie 892, cloudflare-playground 783, aihorde 341, github 303, devin-cli-agentic 223, auto 167, uncloseai 107, codex-app-server 52, cohere 47, zcode 28, anthropic 21, duckduckgo-web 6, trae 4, veoaifree-web 4 → **14 proveedores distintos en chat**. Estado: 502 ×1 826, 400 ×435, 404 ×324, 401 ×131, 429 ×81, **200 ×76 (2,6 %)**, 503 ×71, 499 ×34. Es decir, casi todo es sondeo de proveedores gratuitos que fallan.

**Dentro de CLIProxyAPI (usage_events, 2 749)**: claude 1 173, openai-compatible-omniroutefree 547 (CPA → OmniRoute encadenado), openai-compatible-experientiallab 304, codex 304, freehub (plugin free-model-hub, "dsh") 298, devin 119, openai-compatible-mastra 4 → **7 proveedores**, 5 tipos de executor. `failed=true`: 1 092 (39,7 %). Tokens: 50,7 M entrada / 1,99 M salida. Latencia mediana 4,0 s, p95 25,6 s.

## 3. Concurrencia (barrido de intervalos inicio/fin)

| Medida | Valor |
|---|---|
| Pico de requests en vuelo, omp (todas las sesiones, todos los proveedores) | **16** (2026-11-24 05:25) |
| Pico en vuelo solo gateway-routed desde omp | 15 |
| Pico por proveedor desde omp | anthropic 16, omni 12, xai-oauth 6, openai-codex 6, cliproxyapi 4 |
| Pico en vuelo, OmniRoute chat | **20** (ts=inicio) / 25 (ts=fin), 2026-11-25 ~06:26 |
| Pico en vuelo, CLIProxyAPI | **7–8** |
| Máx. req/s: omp / OmniRoute / CPA | 12 / 23 / 6–7 |
| Máx. req/min: omp / OmniRoute / CPA | 86 / 185 / 56 |
| p95 req/min en minutos activos: omp / OmniRoute / CPA | 26 / 24 / 6 |
| Tiempo ocupado con ≥2 en vuelo (omp) / ≥4 / ≥8 | 77,2 % / 14,2 % / 0,9 % de 18,5 h ocupadas |
| OmniRoute: tiempo con ≥5 / ≥10 en vuelo | 8,4 % / 0,5 % |
| Ráfagas OmniRoute ≥50 rpm | 10 minutos en 7 días (11-23 17:24-17:34 con 81–185 rpm; 11-25 07:56-08:00) |
| Proveedores distintos por minuto (máx.): omp / OmniRoute / CPA | **6** / 7 / 3 (solo 10 minutos con ≥3 proveedores en omp) |
| Proveedores distintos por sesión (máx.) | 8; 24 de 174 sesiones usan >1 |
| Sesiones con requests (7 d) | 174 archivos: 23 de nivel superior + 151 de subagentes; pico de 19 solapadas |
| Sesiones OmniRoute distintas (session_tag/client_session_id) | 538 |
| Procesos omp con evento en logs `omp.*.log` | 16 212 pids (ruido de arranques/plugins, no sesiones) |

## 4. Reintentos y fallbacks

| Evento | Cantidad (7 d) | Fuente |
|---|---|---|
| `model_change` con `reason: fallback` (omp cambió de modelo tras fallo) | **424** (de 2 057 cambios) | sesiones |
| Respuestas assistant con `stopReason=error` | 538 de 11 526 (4,7 %): auth 217, 429 ×66, 502 ×52, quota 12, 503 ×3, otros 177 | sesiones |
| Errores solo en rutas gateway (cliproxyapi+omni+easycliproxy) | 448: auth 217, 429 ×44, 502 ×47, 503 ×3 | sesiones |
| `advisor turn failed` | 141 (91 `auth_unavailable`, 92 con `retry-after-ms`) | omp.log |
| OmniRoute 429 / 502 / 503 en chat | 81 / 1 826 / 71 | call_logs |
| OmniRoute ejecuciones de combo con >1 intento | **132 de 181** (máx. 49 intentos); 22 terminaron en 200 tras fallback | call_logs |
| `Fallback chain … references unknown model` / `retry.fallbackChains key references unknown model` | ≥3 231 avisos (top-25): cadenas de fallback de omp apuntan a modelos inexistentes | omp.log |
| `Mnemopi: … trying the next` (fallback de modelo de memoria) | 132 | omp.log |

Conclusión: el fallback existe y se usa (424 + 132 cadenas de combo), pero hoy lo hace OmniRoute (combos) y omp (cadenas), y parte de la configuración de cadenas en omp está rota (~3 k avisos).

## 5. Tabla de criterios

| Criterio | Medición / evidencia | Rust | Go | Elixir/BEAM |
|---|---|---|---|---|
| Concurrencia real | Pico 16 en vuelo (omp), 20–25 (OmniRoute, ráfagas de 10 min), 7–8 (CPA); máx. 23 req/s. Órdenes de magnitud por debajo de lo que justifica BEAM (miles de procesos). | Sobra | Sobra | Sobra, sin ventaja diferencial |
| Nº de proveedores/conectores | 7 (CPA), 14 (OmniRoute chat), 15 (omp); máx. 6–7 distintos por minuto | tipos + trait: `catalog/provider.rs`, `resolve.rs` ya modelan esto | Ya existe en CPA (`internal/translator`, executors) | Supervisión por conector sería elegante pero innecesaria a esta escala |
| Coste de un runtime extra aquí | Rust: `rustc 1.99.0` instalado (`~/.rustup` 2,9 GB), ya requerido por oh-my-pi (workspace con decenas de crates). Go: `go1.27.2` instalado (270 MB), requerido por CLIProxyAPI. Elixir 1.20.5 + Erlang/OTP 29 instalados vía brew (253 + 11 MB) pero no verifiqué que algún proyecto los use; añadiría VM y release a empaquetar. | 0 extra | 0 extra | Nuevo runtime que mantener |
| Empaquetado para slash command | Un slash command necesita lanzar algo rápido: binario único (CPA ya se distribuye como binario de 70 MB; omp es Rust). BEAM exige release/escript + Erlang en PATH. | Binario único o, mejor, crate enlazada | Binario único | Release (`mix release`) o escript con dependencia de OTP |
| In-process vs servicio `o2ctl services` | `o2ctl services ls` → "No services" (nada registrado hoy). Ya corren 3 daemons externos: OmniRoute (node, 53 MB RSS, 2,7 GB instalado), CLIProxyAPI (Go, 163 MB RSS) y cpa-gui (25 MB). Con ≤25 requests en vuelo, el gateway cabe **in-process** en omp sin servicio adicional; servicio solo si se quiere compartir entre procesos omp. | In-process o servicio | Solo servicio | Solo servicio |
| Encaje con código existente | `crates/catalog` (omp-catalog, 26 360 líneas): `cascade.rs` (42 KB), `policy.rs`, `selection.rs`, `resolve.rs`, `pricing.rs`, `discover.rs` ya cubren cascada/fallback, selección, precios y descubrimiento. `crates/serve/src/inference.rs` (147 KB) ya es el punto de inferencia. CLIProxyAPI = Go (`github.com/router-for-me/CLIProxyAPI/v8`, es un tercero con 8.0.x liberando semanalmente: 13 tarballs descargados). | Reutiliza tipos y política sin FFI | Habría que duplicar el catálogo o llamar a CPA por HTTP | Todo duplicado + frontera de proceso |

## 6. Recomendación

**Rust, como módulo/crate dentro del workspace de oh-my-pi (junto a `omp-catalog`), en proceso; servicio opcional más adelante.** Confianza: **~80 %**.

Razones medidas:
1. La carga es pequeña (pico 16–25 en vuelo, 23 req/s): ningún lenguaje queda limitado; el criterio decisivo pasa a ser coste de integración.
2. 2 636 de 11 526 requests (22,9 %) ya pasan por gateways externos, pero la política de cascada/selección/precios ya vive en `omp-catalog`; reimplementarla en otro lenguaje duplicaría ~26 k líneas.
3. Rust y Go ya están instalados y mantenidos; Elixir/BEAM sería un tercer runtime sin ningún otro consumidor local.
4. El problema observado no es rendimiento sino calidad: 39,7 % de fallos en CPA, 97,4 % de no-200 en OmniRoute, ~3 k avisos de cadenas de fallback inválidas. Un validador de cadenas y selección en `omp-catalog` ataca eso directamente.

Cuándo cambiaría: **Go** si el objetivo fuera contribuir upstream a CLIProxyAPI (ahí es la elección natural, y compartir executors); **Elixir** solo si el gateway pasara a servir muchos clientes simultáneos (cientos+ de conexiones de larga duración), cosa que hoy no ocurre (<1 % del tiempo con ≥8 en vuelo).

Riesgos/caveats: (a) los logs `omp.*.log` no contienen requests; la métrica de omp sale de las sesiones JSONL; (b) `call_logs` de OmniRoute mezcla 641 sondeos `/api/providers/test` y ráfagas de pruebas, por lo que sobrestima el uso real; (c) CPA solo registra lo que pasa por el GUI/plugin. Todos estos sesgos sobrestiman, no subestiman, la concurrencia.
