# premium-discount 공용 라이브러리 (`crates/`)

앱(`apps/core/`)과 raon-zena 가 함께 링크하는 crate 입니다. 앱과 같은 루트 작업 공간(`../Cargo.toml`)의 멤버입니다(`cargo test -p <crate>`, premium-discount 폴더에서. 2026-11-19 에 루트로 합치기 전에는 `crates/Cargo.toml` 이 따로 있던 작업 공간이었습니다). 이름은 약어 없이 `premium-discount-*` 로 씁니다(2026-11-17 `pd-*` 에서 바꿈). 같은 날 raon-zena 의 범용 엔진(센서 · 비전 · 양자화 · NLP · NMS)도 이리로 옮기고, Relay · 창 계산 crate 를 새로 만들었습니다. 2026-11-18 에는 logis-center 의 웹 행동 분석과 tracker-logis-center 를 `premium-discount-analytic` 으로 옮겼습니다.

**원칙 (Tier 0 순수 라이브러리 루트)**
- LanceDB · Tauri · 비동기 런타임(tokio) · 비전(`louis-vision`) 의존성 0건: 앱(`apps/`) 및 전사 비전 채점(`crates/premium-discount-vision-eval`)은 `exclude` 로 분리해 Tier 2-A 에서 `<LV_LIB_PIN_REV>` 와 결합합니다.
- candle 은 전 생태계 공통 `Qntology/candle-rocm-vulkan` 커밋(`f73415d4d149d7949e8a6a94911af670e4e10f7f`)으로 고정합니다.
- 구 `premium-discount-sensor` · `premium-discount-vision` 은 `Qntology/louis-vision/crates/lv-screen` 으로 전량 이관되었고, 순수 2D 기하 원시 타입(`BBox`, `Polar`)만 `premium-discount-sds::geometry` 로 승격되었습니다.
- 범용 평가 코어(`premium-discount-eval`: `rule`, `ranking`, `calib`, `gate_ab`, `accel_tol`, `hygiene`)가 추가되었습니다.
- 네트워크는 `premium-discount-mcp` 의 블로킹 `GET /graph*`(ureq) 하나뿐입니다. `premium-discount-manifest` 는 검증 규칙만 갖고, HTTP 는 부르는 쪽이 고릅니다.
- **도메인 경계**:
  - premium-discount 의 "도메인"은 사용자가 머문 장소 · 대상 · 화면 영역입니다(`cc` · `to` · `Region`). 끝없이 새로 생기는 열린 집합이고, 즉석에서 정규화 · 해시합니다.
  - raon-zena 의 "도메인"은 제나의 표현 사전입니다(`zena_lexicon`, `<group>.<name>`). 큐레이션한 닫힌 집합입니다.
  - 그래서 사전 · 제나의 동작 · 호출어는 raon-zena 에 남깁니다. 이 crate 들은 내용 타입을 제네릭(`DomainBank<M, E>`)으로 받거나, 중립 명세(`DomainSpec` · `VisionDomainSpec`)를 받습니다.

| crate | 역할 | 쓰는 곳 |
| --- | --- | --- |
| `premium-discount-identity` | 시드 구문(BIP-39 12단어) · BIP-32 · EIP-191 서명 · `hashId` · 요청 서명(`x-pd-*`). 기준 `contracts/wallet_v1.json` | nav, schema |
| `premium-discount-verdict` | PD-Verdict 판정(`pd-verdict/1` · `/2`). 기준 `contracts/verdict_v*.json` | nav, mcp, raon-zena `zena-verdict` |
| `premium-discount-nav` | 접근성 내비게이션: 후보 합치기 · 1 ~ 9 번호 · 개인정보 화면 · 마찰 SDS · 타임트라이얼 | mcp(`pd_nav_number`) |
| `premium-discount-schema` | `cc` 정규화 · `to`/`bcc` 주소 · 앱 명령 계약(17개) | app, mcp |
| `premium-discount-filter` | 사전 욕설 필터 `LocalFilter` | app, mcp |
| `premium-discount-mcp-protocol` | stdio JSON-RPC 2.0 MCP (`McpClient` · `serve`) | mcp, raon-zena `zena-avatar-mcp` |
| `premium-discount-mcp` | MCP 서버(`pd_*` 도구 11개) · `PdSyncClient` · `SyncLoop`. [README](premium-discount-mcp/README.md) | raon-zena, 설치본 사이드카 |
| `premium-discount-eval` | 범용 채점 코어(`rule` · `ranking` · `calib` · `gate_ab` · `accel_tol` · `hygiene`) | ecosystem (louis-vision, logis, noto, pd) |
| `premium-discount-sds` | SDS 통계: `Moments` · `Welford` · `GateStat` · `CalibrationStat` · `temporal` · `audit` · `geometry` · `Ledger` | nav, nms, relay, louis-vision, logis, noto |
| `premium-discount-manifest` | 고정 파일 내려받기 검증(`.part` · SHA-256 · 이름 바꾸기 · 안전한 경로) | app, raon-zena 아바타 카탈로그 |
| `onnxruntime-sys` · `onnxruntime` | ONNX Runtime 바인딩(logis-center 판 onnxruntime-rs 0.0.14 벤더 사본, ORT 1.28.0). 기능 `runtime-load` 는 링크 대신 실행할 때 불러옴 | nlp |
| `premium-discount-nlp` | Stanza(ONNX, 12개 언어) · 12개 언어 형태소(`morphology`) · 음차(`phonetic`) · granite-embedding-97m-multilingual-r2 | nms, relay, louis-vision |
| `premium-discount-nms` | 도메인 중립 NLP-NMS: 조사 떼기 · 역할 · 부정(한/영/CJK) · granite/해시 임베더 · `DomainBank` · 구간/시간/공간 NMS | relay, louis-vision, raon-zena |
| `premium-discount-quant` | safetensors · GGUF v3 · imatrix · K-quant 인코더 · 융합 커널 · 어휘 잘라 내기(Track M, ColdGuard 정확 arg-max) | louis-vision, logis-center |
| `premium-discount-relay` | Relay Key 뽑기 · 다국어 키 묶기, 문장 맞추기, 신구 대체(Supersedes) · 중복 · 의미 연결 | (새로 만듦, 아직 부르는 곳 없음) |
| `premium-discount-window` | 오버레이 창 계산(3단계 크기 · 자리 · 누르기와 끌기 · 스타일 비트 · 알파 · 프레임 간격 · 제자리 돌아가기), 점 아래 창 · 앞 창 찾기(Win32) | raon-zena `zena-core::window`, 앱 `web/desktop/stage.ts`(같은 시험 벡터) |
| `premium-discount-analytic` | 웹 행동 분석: 수집 사건(`web-event/1`) · 속성 없는 PUG · 결정론적 시간 해석 · 분석 질의 사전 · 사건 BLOB 코덱 · 퍼널(웹 체크포인트 · 고른 요소 vs 지나친 형제 · 마찰 에피소드). logis-center analytic · tracker-logis-center 를 옮김(README 24절) | 앱 `apps/core/src/analytic.rs` |

**계약 값은 그대로입니다.** crate 이름만 바뀌었고, 아래 값은 서버 · DB · 다른 클라이언트와 맞물려 있어 바꾸지 않았습니다.
- 판정 recipe(`pd-verdict/1` · `pd-verdict/2`)
- 지갑 헤더(`x-pd-address` …)
- MCP 도구 이름(`pd_*`)
- SDS 장부 recipe(`zena-sds-v1:…`)

**raon-zena 에서 쓰기.**
- raon-zena 작업 공간 `Cargo.toml` 이 이 crate 들을 이 저장소의 git 리비전으로 고정합니다. 옆에 둔 체크아웃으로 빌드하려면 그 파일 끝의 `[patch]` 주석을 풉니다.
- raon-zena 쪽에는 사전 · 제나 전용 접착부만 남았습니다(`nlp-nms-router` · `raon-vision` · 얇은 `raon-quant` · `zena-sds` 다시 내보내기).

**데스크톱 설치본의 사이드카.** `apps/desktop/tauri.sidecar.conf.json` 으로 묶으면 `premium-discount-app`(헤드리스 코어)과 `premium-discount-mcp` 가 창 실행 파일 옆에 설치됩니다(`apps/desktop/sidecars.mjs`, README 22절).
