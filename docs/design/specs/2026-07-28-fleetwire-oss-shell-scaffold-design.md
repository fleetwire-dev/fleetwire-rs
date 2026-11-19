# fleetwire OSS shell 스캐폴딩 — 설계

> **날짜** 2026-07-28 · **서브프로젝트** F(OSS 릴리스)의 첫 단계 = **프로젝트 shell**
> **입력** [OSS 릴리스 best-practice 조사](2026-07-28-fleetwire-oss-release-best-practices.md)(P0 체크리스트)
> **선행** [읽기 클라이언트 설계 스펙](2026-07-24-fleetwire-read-client-design.md) · [SDD](fleetwire-read-client/)
> **상태** 설계 승인(2026-07-28) → **검토 반영 개정(rev2)**, 구현 계획 대기

## 0. 목표와 비목표

**목표**: fleetwire를 담을 **별도 신규 OSS 레포의 완전한 shell**을 만든다 — 라이선스·거버넌스·CI·공급망 정책 파일 + 빈 crate 스켈레톤. **첫 커밋에 파일 세트 완비 + 로컬에서 CI와 동일한 명령이 실제로 통과**하는 상태.

> ⚠️ rev2: "CI green + 공개 가능"이라는 표현을 쓰지 않는다. remote가 없으면 GitHub Actions는 실행되지 않으므로 이 단계의 성공 기준은 **로컬 실증**이다. 원격 CI 실행·공개는 §4의 "공개 전 게이트"로 분리한다.

**비목표(이 단계 아님)**:
- A 읽기 라이브러리 로직(엔드포인트 타입·인증·redaction 구현) — 별도 사이클
- fleetwire-mcp 크레이트(B) · 명령 계층(C) · Fleet Telemetry(D)
- crates.io 실제 publish · Trusted Publishing 설정(첫 publish 후에만 가능)
- GitHub remote 연결·원격 CI 실행·브랜치 보호·기여자 대상 P1/P2 파일 (→ §4 공개 전 게이트)

## 1. 확정된 결정 (brainstorming + 검토)

| # | 결정 | 근거 |
|---|------|------|
| D1 | **별도 신규 레포** `<workspace>`(신규 `git init`, main, **remote 없음**) | private 개인 레포(`fleetapi`, 토큰·셋업)에서 공개 OSS 분리 |
| D2 | **범위 = shell만** | A→B→F 각자 사이클. shell은 A에 비의존 |
| D3 | **접근 A(P0 완전 shell 한 번에)** | 조사 문서 P0와 정합 |
| D4 | **workspace-from-start**(멤버 lib 1개) | 미래 mcp/cmd 대비 |
| D5 | `edition="2024"`, `resolver="3"`, `rust-version="1.85.0"`(초기 draft) | edition 2024 floor=1.85. **dep 추가 후 CI 실증값으로 확정** — 현재는 draft MSRV |
| D6 | `version="0.1.0"` + **`publish = false`**(shell 가드) | 자동+수동 오배포 둘 다 기계적 차단. A 완성·publish 준비 시 제거 |
| **D7** | author/저작권 신원 | ✅확정(2026-07-28): owner=**draken1**, 저작권=Jeong Seung Hwan, 커밋 author=noreply `4435353+draken1@users.noreply.github.com`, 연락=**GitHub PVR(무이메일)** |

## 2. 레포 파일 트리

```
fleetwire/
├── Cargo.toml                      # [workspace] members=["crates/fleetwire"], resolver="3"
│                                   # [workspace.package] edition/rust-version/license/repository/homepage/authors
│                                   # [workspace.lints.rust] unsafe_code="forbid"
├── Cargo.lock                      # ★커밋 — 1.85 cargo로 `cargo +1.85 generate-lockfile` 후 (§3 --locked 정합)
├── crates/
│   └── fleetwire/
│       ├── Cargo.toml              # name="fleetwire", 메타 상속, publish=false(shell)
│       │                          # [lints] workspace=true  ★(unsafe_code=forbid 실제 상속)
│       │                          # readme="README.md" ★crate 내부 사본(cargo는 crate 밖 파일 미포함 — §6 R3)
│       │                          # description/keywords/categories + [package.metadata.docs.rs] all-features=true
│       └── src/
│           └── lib.rs              # #![deny(missing_docs)] #![forbid(unsafe_code)]
│                                   # //! 비공식·읽기전용(GET-only)·과금 인식 범위 선언
│                                   # pub 항목 0개. ★cfg_attr(docsrs,feature(doc_cfg))는 넣지 않음
│                                   #   (pub conditional API 없는 지금은 무용 — A에서 도입)
├── LICENSE-MIT                     # 표준 원문 + 저작권 라인 채움(연도 2026, D7 신원)
├── LICENSE-APACHE                  # ★Apache-2.0 원문 verbatim(수정 금지). 저작권 표기는 README/appendix
├── README.md                       # §3.2 순서: 한줄 → 비제휴 고지 → 읽기전용 → 과금경고 → WIP 상태
│                                   #   → 라이선스. ★배지는 CI만(crates.io/docs.rs는 첫 publish 후)
├── CHANGELOG.md                    # Keep a Changelog, [Unreleased]
├── CONTRIBUTING.md                 # setup·fmt·clippy·test·mock·changelog·보안제보·듀얼라이선스 기여문구
├── CODE_OF_CONDUCT.md              # Contributor Covenant 2.1 + ★실제 enforcement 연락처(D7 신원)
├── SECURITY.md                     # ★GitHub PVR 채널(D7, 무이메일) + 지원버전·응답기대. PVR 활성화는 §4 공개 전 게이트
├── .github/
│   ├── workflows/ci.yml            # §3
│   ├── ISSUE_TEMPLATE/{bug_report,feature_request,config}.yml   # config: 보안 링크(GitHub PVR)
│   ├── PULL_REQUEST_TEMPLATE.md
│   └── dependabot.yml              # cargo + github-actions
├── deny.toml · rust-toolchain.toml · rustfmt.toml · .gitignore
```

## 3. CI 워크플로 (`.github/workflows/ci.yml`)

- **트리거**: PR + main push. `permissions: contents: read`, concurrency 취소.
- **actions 풀 40자 SHA 핀** + `persist-credentials: false`.
- **Cargo.lock 정합(M1)**: `Cargo.lock`을 커밋하고 **모든 해석 잡이 `--locked`** 사용. 일관성 위해 fmt 제외 전 잡 `--locked`.
- **toolchain override 방지(M3)**: rust-toolchain.toml이 channel을 고정하므로 **각 잡은 명시적 `cargo +<toolchain>`** 로 실행(설치만 하고 plain `cargo` 호출 금지). 각 잡 시작에 `rustc --version --verbose` 출력.
- **jobs**:
  - `fmt` — `cargo +stable fmt --all -- --check`
  - `test-matrix` (stable·beta·1.85) — `cargo +${{matrix}} test --workspace --all-targets --all-features --locked`
  - `clippy` — `cargo +stable clippy --workspace --all-targets --all-features --locked -- -D warnings`
  - `doc` — `RUSTDOCFLAGS="-D warnings" cargo +stable doc --workspace --all-features --no-deps --locked`
  - `msrv` — `cargo +1.85 check --workspace --all-targets --all-features --locked` (S4)
  - `supply-chain` — cargo-deny action(버전 핀) `check advisories bans licenses sources`
- **연기(→§4)**: release/publish 워크플로·Trusted Publishing(첫 publish 후), Scorecard, coverage, minimal-versions.

> ⚠️ 빈 crate라도 위 잡이 **자동으로** green이지 않다(M1). Cargo.lock 커밋·+toolchain 명시·deny.toml 유효성이 전제. 구현 시 **로컬에서 위 명령을 그대로 실행해 실증**한다.

## 4. 단계 분리 (조사 근거)

**이번 shell 단계 산출**: §2 파일 세트 + 로컬 CI 명령 실증.

**공개 전 게이트(remote 연결 시 묶어서, M4)**:
- GitHub remote 생성·원격 CI 실제 green 확인·브랜치 보호·required checks
- GitHub Private Vulnerability Reporting 활성화(SECURITY.md가 가리키는 기능)
- secret scanning + push protection·**최초 공개 전 staged diff + 시크릿 스캔**(.gitignore는 보안통제 아님)

**후속 사이클**:
| 항목 | 시점 |
|------|------|
| A 읽기 라이브러리 로직 | A 사이클(O1 증거매트릭스 선행) |
| fleetwire-mcp 크레이트 | B 사이클 |
| release-plz + Trusted Publishing 워크플로 | 첫 publish 부트스트랩(§6 재검증) |
| CODEOWNERS · cargo-vet · coverage · minimal-versions · Scorecard · cargo-hack | P1/P2 |

## 5. 완료 정의 (이 shell 단계)

- [ ] `<workspace>` 신규 레포, 첫 커밋(author = D7 신원)
- [ ] **`Cargo.lock` 커밋됨**(`cargo +1.85 generate-lockfile`)
- [ ] 로컬에서 CI와 **동일 명령** 통과: `cargo +stable fmt --all -- --check` / `cargo +stable clippy --workspace --all-targets --all-features --locked -- -D warnings` / `cargo +stable test --workspace --all-targets --all-features --locked` / `RUSTDOCFLAGS="-D warnings" cargo +stable doc --workspace --all-features --no-deps --locked` / `cargo +1.85 check --workspace --all-targets --all-features --locked` / `cargo deny check`
- [ ] `crates/fleetwire/Cargo.toml`에 **`publish = false`** 확인
- [ ] **`cargo package -p fleetwire --list`** 로 tarball에 README·LICENSE-MIT·LICENSE-APACHE 포함 확인(S3), 시크릿·불필요 파일 없음
- [ ] 트리 §2 전부 존재, README에 비제휴·읽기전용·과금·WIP 명시, 배지는 CI만
- [ ] `.gitignore`로 토큰/`.env` 제외 + **staged diff에 시크릿 없음 육안 확인**
- [ ] `[lints] workspace=true`로 unsafe_code=forbid 실제 상속 확인

> "CI green 가정"은 완료 조건이 아니다(M4). 원격 CI 실제 green은 §4 공개 전 게이트.

## 6. 리스크·재검증·주의

- **R1 lockfile↔MSRV 호환(M1)**: `Cargo.lock`을 1.85 cargo로 생성하고, 최신 cargo가 갱신하면 1.85가 못 읽을 수 있음. dep 추가(A) 후 MSRV 잡이 먼저 깨질 수 있으니 **MSRV는 draft이고 CI로 실증**한다. 로컬 rustc는 1.97.1(raw: `rustc --version` = `rustc 1.97.1 (8bab26f4f 2026-07-14)`).
- **R2 toolchain override(M3)**: rust-toolchain.toml channel 고정 + plain `cargo`면 매트릭스가 무의미. §3처럼 **명시적 +toolchain** 필수.
- **R3 nested-crate 패키징(S3)**: cargo는 **crate 디렉토리 밖 파일을 tarball에 넣지 못한다**. 멤버가 `crates/fleetwire/`이므로 root의 README/LICENSE는 published `.crate`에 안 들어감. **기본안**: `crates/fleetwire/` 안에 `README.md`·`LICENSE-MIT`·`LICENSE-APACHE`를 두고(root에도 동일 파일 유지 — root는 GitHub 표시용), 또는 `license`는 SPDX라 파일 없이도 되나 crates.io 페이지 신뢰 위해 crate 내부 사본 권장. `cargo package -p fleetwire --list`로 최종 포함 검증.
- **R4 Apache-2.0 원문(S9)**: LICENSE-APACHE 텍스트는 **verbatim**(수정 금지). 저작권 연도·홀더는 README 또는 appendix에. MIT는 원문의 저작권 라인만 채움.
- **R5 doc_cfg(S8)**: pub conditional API가 생기기 전(A)까진 `cfg_attr(docsrs,feature(doc_cfg))` 넣지 않음. 도입 시 "stable+`--cfg docsrs`면 실패" 명시.
- **R6 재검증 대상([unverifiable], 실제 publish 직전)**: TP 부트스트랩(미발행 crate는 최초 토큰 publish 필요) · doc_cfg 안정화 · **1.97 lockfile→1.85 호환** · crates.io `fleetwire` 이름 가용 · docs.rs nightly 상태.
- **R7 재현성 한계**: `ubuntu-latest` 러너 이미지·advisory DB·다운로드 도구는 가변. SHA 핀은 action tag 변조만 막음 — "완전 재현"이라 표현하지 않음.

## 참고
- best-practice 조사(P0 상세·재확인): [2026-07-28-fleetwire-oss-release-best-practices.md](2026-07-28-fleetwire-oss-release-best-practices.md)
