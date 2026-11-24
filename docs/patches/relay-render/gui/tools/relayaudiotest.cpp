// SPDX-License-Identifier: AGPL-3.0-only
#define SDL_MAIN_HANDLED
#include <SDL.h>
#include "relayaudio.h"
#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <thread>

static unsigned checks = 0;
static void check(bool ok, const char *what) {
    ++checks;
    if (!ok) { std::fprintf(stderr, "FAIL: %s\n", what); std::exit(1); }
}

static void policies() {
    for (unsigned rate : {44100u, 48000u, 96000u})
        for (unsigned channels : {1u, 2u, 6u})
            for (unsigned period : {256u, 512u, 2048u}) {
                RelayAudioBuffer queue;
                queue.reset(rate, channels, period);
                const size_t target = std::max<size_t>(rate / 50, period);
                std::vector<int16_t> input(target * channels * 4, 12345);
                std::vector<int16_t> output(period * channels);
                queue.push(input.data(), input.size() * 2, 100);
                auto s = queue.snapshot();
                check(s.queued_frames == target, "burst keeps latest target, no backlog");
                check(s.dropped_frames == target * 3, "exact dropped frame accounting");
                queue.read(output.data(), output.size() * 2, 101);
                check(output.back() == 12345, "all formats preserve S16 values after de-click");
                queue.read(output.data(), output.size() * 2, 300);
                check(std::all_of(output.begin(), output.end(), [](int16_t x) { return x == 0; }), "expired data is silent");
                s = queue.snapshot();
                check(s.queued_frames == 0, "stall discards stale remainder");
            }

    RelayAudioBuffer q;
    q.reset(48000, 2, 512);
    std::vector<int16_t> pcm(960 * 2);
    for (int i = 0; i < 960; ++i) { pcm[i*2] = int16_t(i+100); pcm[i*2+1] = -pcm[i*2]; }
    q.push(pcm.data(), pcm.size()*2, 0);
    std::vector<int16_t> out(512*2);
    q.read(out.data(), out.size()*2, 1);
    check(std::equal(out.begin()+96, out.end(), pcm.begin()+96), "normal PCM is bit exact after 1 ms ramp");
    auto before = q.snapshot();
    q.push(pcm.data(), 3, 2);
    check(q.snapshot().queued_frames == before.queued_frames, "partial sample frame rejected");
    q.read(out.data(), out.size()*2, 3);
    check(q.snapshot().underruns == 1, "underrun counted");
    check(std::all_of(out.begin()+448*2, out.end(), [](int16_t x){return x==0;}), "underrun tail is initialized silence");
    check(out[447*2] == 0 && out[447*2+1] == 0, "underrun fades to silence");

    // Simulate independent network/device clocks for two minutes, including
    // 200 ms producer bursts and device suspension. No live console required.
    q.reset(48000, 2, 512);
    std::vector<int16_t> packet(480*2);
    int64_t callbacks = 0;
    for (int64_t ms = 0; ms < 120000; ++ms) {
        if (ms % 10 == 0) {
            std::fill(packet.begin(), packet.end(), int16_t(ms/10));
            if (ms % 5000 == 0) for (int burst=0; burst<20; ++burst) q.push(packet.data(),packet.size()*2,ms);
            q.push(packet.data(),packet.size()*2,ms);
        }
        if ((callbacks * 512000 / 48000) <= ms) {
            if (ms % 7000 >= 200) q.read(out.data(),out.size()*2,ms);
            ++callbacks;
        }
        const auto s=q.snapshot();
        check(s.queued_frames <= s.capacity_frames, "bounded across clock/jitter/stall simulation");
    }
    check(q.snapshot().dropped_frames > 0, "simulated stalls actually recover by dropping stale audio");
}

static void device() {
    check(SDL_Init(SDL_INIT_AUDIO)==0, "SDL audio init");
    RelayAudioBuffer q;
    SDL_AudioSpec want{},got{};
    want.freq=48000; want.channels=2; want.format=AUDIO_S16SYS; want.samples=512;
    want.userdata=&q;
    want.callback=[](void *user, Uint8 *data,int bytes){static_cast<RelayAudioBuffer*>(user)->read(data,bytes);};
    const auto dev=SDL_OpenAudioDevice(nullptr,0,&want,&got,0);
    check(dev!=0,"real device callback opens");
    check(got.freq==48000 && got.channels==2 && got.format==AUDIO_S16SYS,"S16 stereo format retained");
    q.reset(got.freq,got.channels,got.samples);
    std::vector<int16_t> silence(480*2,0);
    q.push(silence.data(),silence.size()*2);
    q.push(silence.data(),silence.size()*2);
    SDL_PauseAudioDevice(dev,0);
    const auto start=std::chrono::steady_clock::now();
    for(int i=0;i<500;++i){
        // Deliberately block the feeder for 150 ms once. Playback must resume
        // without retaining 150 ms of queued sound. Output is silence only.
        if(i==200)std::this_thread::sleep_for(std::chrono::milliseconds(150));
        q.push(silence.data(),silence.size()*2);
        std::this_thread::sleep_until(start+std::chrono::milliseconds((i+1)*10));
    }
    SDL_PauseAudioDevice(dev,1);
    const auto s=q.snapshot();
    check(s.callbacks>100 && s.played_frames>100000,"real audio callback consumed PCM");
    check(s.peak_frames<=s.capacity_frames,"device queue remained bounded");
    check(SDL_GetQueuedAudioSize(dev)==0,"no second SDL playback queue");
    std::printf("{\"driver\":\"%s\",\"devicePeriodMs\":%.3f,\"peakQueueMs\":%.3f,\"capacityMs\":%.3f,\"callbacks\":%llu,\"underruns\":%llu,\"droppedFrames\":%llu}\n",
        SDL_GetCurrentAudioDriver(),1000.0*s.period_frames/s.rate,1000.0*s.peak_frames/s.rate,1000.0*s.capacity_frames/s.rate,
        (unsigned long long)s.callbacks,(unsigned long long)s.underruns,(unsigned long long)s.dropped_frames);
    SDL_CloseAudioDevice(dev);SDL_Quit();
}

int main(int argc,char **argv) {
    policies();
    if(argc>1 && std::strcmp(argv[1],"--device")==0)device();
    std::printf("PASS: %u audio checks\n",checks);
    return 0;
}
