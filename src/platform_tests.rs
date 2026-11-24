//! Isolated Win32/WASAPI integration fixtures. No console, camera, microphone or real engine is opened.
use super::*;
use std::{
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
};
use windows::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, LRESULT, WPARAM},
        Graphics::Gdi::ClientToScreen,
        Media::Audio::{
            eConsole, IAudioClient, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_NOPERSIST,
        },
        System::Com::{CoCreateGuid, CoTaskMemFree},
    },
};

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
}
const FIXTURE: &str = "RELAY_TEST_FIXTURE_MODE";
struct Fixture(Child);
impl Fixture {
    fn spawn(mode: &str) -> Self {
        let cmd = Command::new(std::env::current_exe().unwrap())
            .args(["fixture_child_process", "--ignored", "--test-threads=1"])
            .env(FIXTURE, mode)
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        Self(cmd)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Host(HWND);
impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
unsafe extern "system" fn fixture_proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return LRESULT(0);
    }
    if msg == WM_TIMER {
        unsafe {
            let _ = DestroyWindow(h);
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(h, msg, w, l) }
}
fn window_rect(h: HWND) -> RECT {
    let mut r = RECT::default();
    unsafe {
        GetWindowRect(h, &mut r).unwrap();
    }
    r
}
fn relative_rect(host: HWND, h: HWND) -> PixelRect {
    let r = window_rect(h);
    let mut origin = POINT::default();
    assert!(unsafe { ClientToScreen(host, &mut origin).as_bool() });
    (
        r.left - origin.x,
        r.top - origin.y,
        r.right - r.left,
        r.bottom - r.top,
    )
}
fn create_silent_output(
    devices: &IMMDeviceEnumerator,
) -> WinResult<(IAudioClient, ISimpleAudioVolume)> {
    // Allocate a non-persistent render session identified by a fresh GUID, without starting
    // the output stream or writing a single sample. Its volume does not affect any user app.
    unsafe {
        let endpoint = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client = endpoint.Activate::<IAudioClient>(CLSCTX_ALL, None)?;
        let guid = CoCreateGuid()?;
        let format = client.GetMixFormat()?;
        let result = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_NOPERSIST,
            0,
            0,
            format,
            Some(&guid),
        );
        CoTaskMemFree(Some(format.cast()));
        result?;
        let volume = client.GetService::<ISimpleAudioVolume>()?;
        Ok((client, volume))
    }
}
#[test]
#[ignore = "internal subprocess fixture"]
fn fixture_child_process() {
    match std::env::var(FIXTURE).ok().as_deref() {
        Some("video" | "qt" | "qt-pending") => unsafe {
            let pending = std::env::var(FIXTURE).as_deref() == Ok("qt-pending");
            let class = if std::env::var(FIXTURE).as_deref() != Ok("video") {
                w!("Qt681QWindowIcon")
            } else {
                w!("RelayPlaybackWindow")
            };
            let instance = GetModuleHandleW(std::ptr::null());
            let atom = RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(fixture_proc),
                hInstance: instance,
                lpszClassName: class,
                ..Default::default()
            });
            assert_ne!(atom, 0);
            let h = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("Relay test"),
                WS_POPUP | WS_THICKFRAME | WS_SYSMENU,
                -14000,
                -14000,
                375,
                250,
                None,
                None,
                instance,
                None,
            )
            .unwrap();
            if pending {
                SetPropW(
                    h,
                    w!("RelayPlayer.VideoReady.V1"),
                    windows::Win32::Foundation::HANDLE(std::process::id() as usize as *mut _),
                )
                .unwrap();
            } else {
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            }
            SetTimer(h, 1, 15000, None);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnregisterClassW(class, instance);
        },
        Some("audio") => {
            let _com = ComApartment::new().unwrap();
            let devices: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap() };
            let (_client, mixer) = create_silent_output(&devices).unwrap();
            unsafe {
                mixer.SetMasterVolume(0.731, std::ptr::null()).unwrap();
            }
            // Wait only within the helper. The parent Drop kills/reaps this disposable process.
            thread::sleep(Duration::from_secs(15));
        }
        _ => {}
    }
}
#[test]
fn embeds_a_separate_process_only_in_its_video_area_and_restores_it() {
    check_embedding("video", false);
}
#[test]
fn embeds_exact_owned_qt_stream_and_restores_it_without_accepting_foreign_pid() {
    check_embedding("qt", true);
}
#[test]
fn adopts_hidden_owned_qt_surface_before_showing_video() {
    check_embedding("qt-pending", true);
}
fn check_embedding(mode: &str, public: bool) {
    let mut child = Fixture::spawn(mode);
    let pid = child.0.id();
    let deadline = Instant::now() + Duration::from_secs(6);
    let video = loop {
        if let Some((h, _)) = find_owned_video(pid, public).unwrap() {
            break h;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "fixture exited before creating its window"
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for fixture window"
        );
        thread::sleep(Duration::from_millis(30));
    };
    assert!(find_video(u32::MAX).unwrap().is_none());
    assert!(find_owned_video(u32::MAX, public).unwrap().is_none());
    assert!(!matches_owned(video, std::process::id(), public));
    let host = Host(unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!("off-screen test host"),
            WS_POPUP,
            -15500,
            -15500,
            600,
            400,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    });
    let original_style = style_bits(video, GWL_STYLE);
    let original_ex = style_bits(video, GWL_EXSTYLE);
    let original_rect = window_rect(video);
    let mut pane = PlayerSurface::new(host.0 .0 as isize);
    pane.set_public_runtime(public);
    assert!(
        pane.tick(pid, true, Some((30, 40, 320, 180)), 1.).embedded,
        "{:?}",
        pane.status
    );
    assert_eq!(parent_of(video), host.0);
    assert_ne!(style_bits(video, GWL_STYLE) & WS_CHILD.0, 0);
    // A foreground-PID check sees the Rust host as the foreground process for this
    // embedded child; it cannot see the child engine as the foreground owner.
    let foreground_root = unsafe { GetAncestor(video, GA_ROOT) };
    assert_eq!(foreground_root, host.0);
    assert_eq!(pid_of(foreground_root), std::process::id());
    assert_eq!(relative_rect(host.0, video), (30, 40, 320, 180));
    // Simulate resizing around top/bottom chrome. Source is clipped at the host bounds.
    pane.tick(pid, true, Some((-10, 42, 820, 440)), 1.);
    assert_eq!(relative_rect(host.0, video), (0, 42, 600, 358));
    pane.tick(pid, true, None, 1.);
    assert_eq!(style_bits(video, GWL_STYLE) & WS_VISIBLE.0, 0);
    pane.tick(pid, true, Some((6, 7, 250, 140)), 1.);
    assert_ne!(style_bits(video, GWL_STYLE) & WS_VISIBLE.0, 0);
    assert_eq!(relative_rect(host.0, video), (6, 7, 250, 140));
    // The restored top-level window remains independent on subsequent frames.
    // The synthetic pending fixture must not put a real independent window on
    // the desktop. Its hidden state is checked again below after detaching.
    if mode != "qt-pending" {
        pane.set_native_window(true);
        for _ in 0..2 {
            let s = pane.tick(pid, true, Some((6, 7, 250, 140)), 0.7);
            assert!(s.native_window && !s.embedded);
            assert!(parent_of(video).is_invalid());
            assert_eq!(style_bits(video, GWL_STYLE) & WS_CHILD.0, 0);
            assert_eq!(pid_of(unsafe { GetAncestor(video, GA_ROOT) }), pid);
        }
        pane.set_native_window(false);
        assert!(pane.tick(pid, true, Some((6, 7, 250, 140)), 1.).embedded);
    }
    assert!(!pane.tick(0, false, None, 1.).embedded);
    assert!(parent_of(video).is_invalid());
    assert_eq!(style_bits(video, GWL_STYLE), original_style);
    assert_eq!(style_bits(video, GWL_EXSTYLE), original_ex);
    let restored = window_rect(video);
    assert_eq!(
        (
            restored.right - restored.left,
            restored.bottom - restored.top
        ),
        (
            original_rect.right - original_rect.left,
            original_rect.bottom - original_rect.top
        )
    );
    pane.shutdown();
    assert_eq!(style_bits(host.0, GWL_STYLE) & WS_CLIPCHILDREN.0, 0);
    unsafe {
        PostMessageW(video, WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(code) = child.0.try_wait().unwrap() {
            assert!(code.success());
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(15));
    }
}
#[test]
fn never_embeds_another_process_even_when_its_window_class_matches() {
    let mut engine = Fixture::spawn("video");
    let engine_pid = engine.0.id();
    let mut playback = Fixture::spawn("video");
    let playback_pid = playback.0.id();
    let deadline = Instant::now() + Duration::from_secs(6);
    let (engine_window, playback_window) = loop {
        let engine_window = find_video(engine_pid).unwrap();
        let playback_window = find_video(playback_pid).unwrap();
        if let (Some(engine_window), Some(playback_window)) = (engine_window, playback_window) {
            break (engine_window.0, playback_window.0);
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for fixture windows"
        );
        thread::sleep(Duration::from_millis(30));
    };
    let host = Host(unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            w!("STATIC"),
            w!("off-screen process selection host"),
            WS_POPUP,
            -15500,
            -15500,
            600,
            400,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    });
    let mut pane = PlayerSurface::new(host.0 .0 as isize);
    let status = pane.tick(engine_pid, true, Some((20, 30, 320, 180)), 1.);
    assert!(status.embedded && !status.native_window, "{status:?}");
    assert_eq!(parent_of(engine_window), host.0);
    assert!(parent_of(playback_window).is_invalid());
    pane.set_native_window(true);
    let status = pane.tick(engine_pid, true, Some((20, 30, 320, 180)), 1.);
    assert!(status.native_window && !status.embedded);
    assert_eq!(pane.external, Some((engine_window, engine_pid)));
    assert!(parent_of(playback_window).is_invalid());

    pane.shutdown();
    unsafe {
        let _ = PostMessageW(engine_window, WM_CLOSE, WPARAM(0), LPARAM(0));
        let _ = PostMessageW(playback_window, WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    for fixture in [&mut engine, &mut playback] {
        loop {
            if let Some(code) = fixture.0.try_wait().unwrap() {
                assert!(code.success());
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(15));
        }
    }
}

#[test]
#[ignore = "requires an active Windows audio output; uses two silent disposable render sessions"]
fn volume_updates_the_selected_process_and_leaves_other_output_alone() {
    let _com = ComApartment::new().unwrap();
    let devices: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap() };
    let (_own, local) = create_silent_output(&devices).unwrap();
    let mut child = Fixture::spawn("audio");
    let deadline = Instant::now() + Duration::from_secs(7);
    let other = loop {
        let vols = discover_audio(
            &devices,
            AudioTarget {
                pid: child.0.id(),
                level: 1000,
            },
        )
        .unwrap();
        if let Some(v) = vols
            .into_iter()
            .find(|v| unsafe { v.GetMasterVolume() }.is_ok_and(|n| (n - 0.731).abs() < 0.003))
        {
            break v;
        }
        assert!(child.0.try_wait().unwrap().is_none());
        assert!(
            Instant::now() < deadline,
            "timed out waiting for silent fixture"
        );
        thread::sleep(Duration::from_millis(40));
    };
    let mut pane = PlayerSurface::new(0);
    let pid = std::process::id();
    for level in [0.41, 0.0, 0.88] {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let count = pane.tick(pid, true, None, level).audio_sessions;
            if count > 0 && (unsafe { local.GetMasterVolume() }.unwrap() - level).abs() < 0.003 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "level {level} never applied: {:?}",
                pane.status
            );
            thread::sleep(Duration::from_millis(25));
        }
        assert!((unsafe { other.GetMasterVolume() }.unwrap() - 0.731).abs() < 0.003);
    }
    pane.shutdown();
}
