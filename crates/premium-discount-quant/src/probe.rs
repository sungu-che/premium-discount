//! 가속기 자가 검증 프로브를 프로세스를 죽이지 않고 돌린다.
//!
//! `catch_unwind` 는 패닉이 풀릴(unwind) 때만 프로브를 격리한다. 릴리스 프로필이 `panic = "abort"` 인 앱(louis-vision
//! apps/louis, logis-center)에서는 프로브 안의 패닉이 곧바로 프로세스 종료(SIGABRT)다. 그래서 결함 주입은 프로브를 부르기
//! 전에 정적 분기로 `Err` 를 돌려주고(패닉을 쓰지 않는다), 그 밖의 패닉은 풀리는 빌드에서만 잡는다.

use std::panic::{catch_unwind, AssertUnwindSafe};

/// 이 빌드에서 패닉이 풀리는가 (`panic = "abort"` 가 아님). 거짓이면 `catch_unwind` 로 잡을 것이 없다.
pub const PANIC_UNWINDS: bool = !cfg!(panic = "abort");

/// 프로브가 기준을 통과하지 못한 이유
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure<E> {
    /// 결함 주입: 프로브를 부르지 않고 오류로 돌려줬다
    Injected,
    /// 프로브가 오류를 돌려줬다
    Failed(E),
    /// 프로브가 패닉했고 잡았다 (패닉이 풀리는 빌드에서만)
    Panicked,
}

/// 가속기 연산 하나의 자가 검증을 돌린다.
///
/// - `inject_fault` 가 참이면 `probe` 를 부르지 않고 [`ProbeFailure::Injected`] (시험 · 폴백 경로 확인용).
/// - 패닉이 풀리는 빌드: `probe` 의 패닉을 [`ProbeFailure::Panicked`] 로 바꾼다.
/// - `panic = "abort"` 빌드: 잡을 수 없으므로 그냥 부른다. 이런 빌드에서 결함 주입을 패닉으로 하면 안 되는 까닭이다.
pub fn run_probe<T, E>(inject_fault: bool, probe: impl FnOnce() -> Result<T, E>) -> Result<T, ProbeFailure<E>> {
    if inject_fault {
        return Err(ProbeFailure::Injected);
    }
    if PANIC_UNWINDS {
        match catch_unwind(AssertUnwindSafe(probe)) {
            Ok(r) => r.map_err(ProbeFailure::Failed),
            Err(_) => Err(ProbeFailure::Panicked),
        }
    } else {
        probe().map_err(ProbeFailure::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn injected_faults_never_call_the_probe_and_panics_are_caught_when_unwinding() {
        let called = Cell::new(false);
        let r: Result<u8, ProbeFailure<&str>> = run_probe(true, || {
            called.set(true);
            Ok(1)
        });
        assert_eq!(r, Err(ProbeFailure::Injected));
        assert!(!called.get(), "결함 주입은 프로브 앞의 정적 분기다");

        assert_eq!(run_probe::<u8, &str>(false, || Ok(7)), Ok(7));
        assert_eq!(run_probe::<u8, &str>(false, || Err("rel_l2 > 5e-3")), Err(ProbeFailure::Failed("rel_l2 > 5e-3")));

        // 시험 프로필은 언제나 패닉이 풀린다
        assert!(PANIC_UNWINDS);
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = run_probe::<u8, &str>(false, || panic!("kernel fault"));
        std::panic::set_hook(prev);
        assert_eq!(r, Err(ProbeFailure::Panicked));
    }
}
