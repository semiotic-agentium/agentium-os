"""
Agentium OS YTP Explainer Video — Main Render Orchestrator

Usage:
    python -m video.render [--scene 1] [--output output/scene_01.mp4] [--no-audio]
"""

from __future__ import annotations

import argparse
import os
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
from PIL import Image

# Ensure video package is importable from workspace root
sys.path.insert(0, str(Path(__file__).parent.parent))


def _frames_to_video_clip(frames: list[Image.Image], fps: int):
    """Convert PIL frames to a MoviePy ImageSequenceClip."""
    from moviepy.video.io.ImageSequenceClip import ImageSequenceClip

    np_frames = [np.array(f) for f in frames]
    return ImageSequenceClip(np_frames, fps=fps)


def _audio_to_clip(audio_seg, tmp_dir: str):
    """Export pydub AudioSegment to WAV and load as MoviePy AudioFileClip."""
    from moviepy.audio.io.AudioFileClip import AudioFileClip

    wav_path = os.path.join(tmp_dir, "audio.wav")
    audio_seg.export(wav_path, format="wav")
    return AudioFileClip(wav_path)



def _generic_render(
    scene_name: str,
    render_fn,
    audio_fn,
    total_frames: int,
    fps: int,
    output_path: str,
    include_audio: bool = True,
    cache_dir: str | None = None,
    bitrate: str = "8000k",
) -> list[Image.Image]:
    """Generic scene render helper. Returns the frame list."""
    print(f"[RENDER] {scene_name} — {total_frames} frames @ {fps}fps ({total_frames/fps:.1f}s)")
    start = time.time()

    # Try frame cache
    frames: list[Image.Image] | None = None
    if cache_dir and os.path.isdir(cache_dir):
        cached = sorted(Path(cache_dir).glob("frame_*.png"))
        if len(cached) == total_frames:
            print(f"  Loading {total_frames} cached frames from {cache_dir} ...")
            frames = [Image.open(str(p)).convert("RGB") for p in cached]
            print("  Cache loaded.")

    if frames is None:
        def progress(f: int, total: int) -> None:
            if f % 30 == 0 or f == total:
                elapsed = time.time() - start
                fps_actual = f / max(0.001, elapsed)
                eta = (total - f) / max(0.001, fps_actual)
                bar_len = 30
                filled = int(bar_len * f / total)
                bar = "█" * filled + "░" * (bar_len - filled)
                print(f"\r  [{bar}] {f}/{total}  {fps_actual:.1f} fps  ETA {eta:.0f}s  ", end="", flush=True)

        print("  Rendering frames...")
        frames = render_fn(progress_cb=progress)
        print(f"\n  Done in {time.time() - start:.1f}s — {len(frames)} frames rendered")

        if cache_dir:
            os.makedirs(cache_dir, exist_ok=True)
            print(f"  Caching frames to {cache_dir} ...")
            for i, f in enumerate(frames):
                f.save(os.path.join(cache_dir, f"frame_{i:05d}.png"))

    if output_path:
        video_clip = _frames_to_video_clip(frames, fps=fps)
        if include_audio:
            print("  Synthesizing audio...")
            audio_seg = audio_fn(fps=fps, total_frames=total_frames)
            with tempfile.TemporaryDirectory() as tmp:
                audio_clip = _audio_to_clip(audio_seg, tmp)
                audio_clip = audio_clip.subclipped(0, min(audio_clip.duration, video_clip.duration))
                final_clip = video_clip.with_audio(audio_clip)
                print(f"  Encoding to {output_path} ...")
                os.makedirs(os.path.dirname(os.path.abspath(output_path)), exist_ok=True)
                final_clip.write_videofile(
                    output_path, fps=fps, codec="libx264", audio_codec="aac",
                    bitrate=bitrate, audio_bitrate="192k", preset="medium",
                    ffmpeg_params=["-pix_fmt", "yuv420p", "-color_range", "pc",
                                   "-colorspace", "bt709", "-color_primaries", "bt709",
                                   "-color_trc", "bt709"],
                    logger=None,
                )
                audio_clip.close()
            final_clip.close()
        else:
            print(f"  Encoding (silent) to {output_path} ...")
            os.makedirs(os.path.dirname(os.path.abspath(output_path)), exist_ok=True)
            video_clip.write_videofile(
                output_path, fps=fps, codec="libx264", bitrate=bitrate,
                preset="medium",
                ffmpeg_params=["-pix_fmt", "yuv420p", "-color_range", "pc",
                               "-colorspace", "bt709", "-color_primaries", "bt709",
                               "-color_trc", "bt709"],
                logger=None,
            )
            video_clip.close()
        size_mb = os.path.getsize(output_path) / 1024 / 1024
        print(f"\n[DONE] {output_path}  ({size_mb:.1f} MB)")

    return frames


def render_scene_01(output_path: str, include_audio: bool = True, cache_dir: str | None = None) -> None:
    from video.scenes.scene_01_other_agents import render_scene_01 as _render, FPS, TOTAL_FRAMES
    from video.audio.sfx import build_scene_01_audio
    _generic_render("Scene One — OTHER AGENTS", _render, build_scene_01_audio,
                    TOTAL_FRAMES, FPS, output_path, include_audio, cache_dir)


def render_scene_02(output_path: str, include_audio: bool = True, cache_dir: str | None = None) -> None:
    from video.scenes.scene_02_agentium_os import render_scene_02 as _render, FPS, TOTAL_FRAMES
    from video.audio.sfx import build_scene_02_audio
    _generic_render("Scene Two — AGENTIUM OS", _render, build_scene_02_audio,
                    TOTAL_FRAMES, FPS, output_path, include_audio, cache_dir)


def render_scene_03(output_path: str, include_audio: bool = True, cache_dir: str | None = None) -> None:
    from video.scenes.scene_03_observability import render_scene_03 as _render, FPS, TOTAL_FRAMES
    from video.audio.sfx import build_scene_03_audio
    _generic_render("Scene Three — OBSERVABILITY", _render, build_scene_03_audio,
                    TOTAL_FRAMES, FPS, output_path, include_audio, cache_dir)


def render_scene_04c(output_path: str, include_audio: bool = True, cache_dir: str | None = None) -> None:
    from video.scenes.scene_04_citations import render_scene_04_citations as _render, FPS, TOTAL_FRAMES
    from video.audio.sfx import build_scene_04_citations_audio
    _generic_render("Scene Four-C — CITATIONS", _render, build_scene_04_citations_audio,
                    TOTAL_FRAMES, FPS, output_path, include_audio, cache_dir)


def render_scene_04(output_path: str, include_audio: bool = True, cache_dir: str | None = None) -> None:
    from video.scenes.scene_04_repository import render_scene_04 as _render, FPS, TOTAL_FRAMES
    from video.audio.sfx import build_scene_04_audio
    _generic_render("Scene Four — REPOSITORY", _render, build_scene_04_audio,
                    TOTAL_FRAMES, FPS, output_path, include_audio, cache_dir)


def render_scene_05(output_path: str, include_audio: bool = True, cache_dir: str | None = None) -> None:
    from video.scenes.scene_05_the_stack import render_scene_05 as _render, FPS, TOTAL_FRAMES
    from video.audio.sfx import build_scene_05_audio
    _generic_render("Scene Five — THE STACK", _render, build_scene_05_audio,
                    TOTAL_FRAMES, FPS, output_path, include_audio, cache_dir)


def render_loop_bg(output_path: str, include_audio: bool = True, cache_dir: str | None = None) -> None:
    from video.scenes.scene_loop_bg import render_loop_bg as _render, FPS, TOTAL_FRAMES
    from video.audio.sfx import build_loop_bg_audio
    _generic_render("Loop BG — 4K VIDEO CALL BACKGROUND", _render, build_loop_bg_audio,
                    TOTAL_FRAMES, FPS, output_path, include_audio=include_audio, cache_dir=cache_dir,
                    bitrate="20000k")


def render_all(output_path: str, include_audio: bool = True,
               cache_dir_s1: str | None = None, cache_dir_s2: str | None = None,
               cache_dir_s3: str | None = None, cache_dir_s4c: str | None = None,
               cache_dir_s4: str | None = None, cache_dir_s5: str | None = None) -> None:
    """Render all scenes (including citations), concatenate into one video."""
    from video.scenes.scene_01_other_agents import render_scene_01 as _r1, FPS as FPS1, TOTAL_FRAMES as TF1
    from video.scenes.scene_02_agentium_os import render_scene_02 as _r2, FPS as FPS2, TOTAL_FRAMES as TF2
    from video.scenes.scene_03_observability import render_scene_03 as _r3, FPS as FPS3, TOTAL_FRAMES as TF3
    from video.scenes.scene_04_citations import render_scene_04_citations as _r4c, FPS as FPS4c, TOTAL_FRAMES as TF4c
    from video.scenes.scene_04_repository import render_scene_04 as _r4, FPS as FPS4, TOTAL_FRAMES as TF4
    from video.scenes.scene_05_the_stack import render_scene_05 as _r5, FPS as FPS5, TOTAL_FRAMES as TF5
    from video.audio.sfx import (build_scene_01_audio, build_scene_02_audio, build_scene_03_audio,
                                   build_scene_04_citations_audio, build_scene_04_audio, build_scene_05_audio)

    frames1  = _generic_render("Scene One",       _r1,  build_scene_01_audio,           TF1,  FPS1,  "", include_audio=False, cache_dir=cache_dir_s1)
    frames2  = _generic_render("Scene Two",       _r2,  build_scene_02_audio,           TF2,  FPS2,  "", include_audio=False, cache_dir=cache_dir_s2)
    frames3  = _generic_render("Scene Three",     _r3,  build_scene_03_audio,           TF3,  FPS3,  "", include_audio=False, cache_dir=cache_dir_s3)
    frames4c = _generic_render("Scene Citations", _r4c, build_scene_04_citations_audio, TF4c, FPS4c, "", include_audio=False, cache_dir=cache_dir_s4c)
    frames4  = _generic_render("Scene Four",      _r4,  build_scene_04_audio,           TF4,  FPS4,  "", include_audio=False, cache_dir=cache_dir_s4)
    frames5  = _generic_render("Scene Five",      _r5,  build_scene_05_audio,           TF5,  FPS5,  "", include_audio=False, cache_dir=cache_dir_s5)

    all_frames = frames1 + frames2 + frames3 + frames4c + frames4 + frames5
    print(f"  Concatenating {len(all_frames)} frames ({len(all_frames)/FPS1:.1f}s total)...")
    video_clip = _frames_to_video_clip(all_frames, fps=FPS1)

    if include_audio:
        from pydub import AudioSegment
        XFADE = 700  # ms crossfade at each scene boundary
        audio1  = build_scene_01_audio(fps=FPS1,  total_frames=TF1).fade_out(XFADE)
        audio2  = build_scene_02_audio(fps=FPS2,  total_frames=TF2).fade_in(XFADE).fade_out(XFADE)
        audio3  = build_scene_03_audio(fps=FPS3,  total_frames=TF3).fade_in(XFADE).fade_out(XFADE)
        audio4c = build_scene_04_citations_audio(fps=FPS4c, total_frames=TF4c).fade_in(XFADE).fade_out(XFADE)
        audio4  = build_scene_04_audio(fps=FPS4,  total_frames=TF4).fade_in(XFADE).fade_out(XFADE)
        audio5  = build_scene_05_audio(fps=FPS5,  total_frames=TF5).fade_in(XFADE)
        combined_audio = audio1 + audio2 + audio3 + audio4c + audio4 + audio5
        with tempfile.TemporaryDirectory() as tmp:
            audio_clip = _audio_to_clip(combined_audio, tmp)
            audio_clip = audio_clip.subclipped(0, min(audio_clip.duration, video_clip.duration))
            final_clip = video_clip.with_audio(audio_clip)
            os.makedirs(os.path.dirname(os.path.abspath(output_path)), exist_ok=True)
            print(f"  Encoding combined video to {output_path} ...")
            _COLOUR_FLAGS = ["-pix_fmt", "yuv420p", "-color_range", "pc",
                             "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]
            final_clip.write_videofile(
                output_path, fps=FPS1, codec="libx264", audio_codec="aac",
                bitrate="8000k", audio_bitrate="192k", preset="medium",
                ffmpeg_params=_COLOUR_FLAGS, logger=None,
            )
            audio_clip.close()
        final_clip.close()
    else:
        os.makedirs(os.path.dirname(os.path.abspath(output_path)), exist_ok=True)
        _COLOUR_FLAGS = ["-pix_fmt", "yuv420p", "-color_range", "pc",
                         "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]
        video_clip.write_videofile(
            output_path, fps=FPS1, codec="libx264", bitrate="8000k",
            preset="medium", ffmpeg_params=_COLOUR_FLAGS, logger=None,
        )
        video_clip.close()

    size_mb = os.path.getsize(output_path) / 1024 / 1024
    print(f"\n[DONE] {output_path}  ({size_mb:.1f} MB)")


def main() -> None:
    parser = argparse.ArgumentParser(description="Agentium OS YTP Explainer Video Renderer")
    parser.add_argument("--scene", type=str, default="0", help="Scene to render (1-5, 'loop', or 0=all)")
    parser.add_argument("--output", default=None, help="Output file path")
    parser.add_argument("--no-audio", action="store_true", help="Render without audio")
    parser.add_argument("--cache-dir", default=None, help="Frame cache dir (scene 1 only)")
    parser.add_argument("--cache-dir-s2", default=None, help="Frame cache dir for scene 2")
    parser.add_argument("--cache-dir-s3", default=None, help="Frame cache dir for scene 3")
    parser.add_argument("--cache-dir-s4", default=None, help="Frame cache dir for scene 4")
    parser.add_argument("--cache-dir-s5", default=None, help="Frame cache dir for scene 5")
    args = parser.parse_args()

    if args.scene == "loop":
        out = args.output or "video/output/agentium_loop_bg.mp4"
        render_loop_bg(out, include_audio=not args.no_audio, cache_dir=args.cache_dir)
        return

    args.scene = int(args.scene)

    if args.scene == 1:
        out = args.output or "video/output/scene_01.mp4"
        render_scene_01(out, include_audio=not args.no_audio, cache_dir=args.cache_dir)
    elif args.scene == 2:
        out = args.output or "video/output/scene_02.mp4"
        render_scene_02(out, include_audio=not args.no_audio, cache_dir=args.cache_dir)
    elif args.scene == 3:
        out = args.output or "video/output/scene_03.mp4"
        render_scene_03(out, include_audio=not args.no_audio, cache_dir=args.cache_dir)
    elif args.scene == 4:
        out = args.output or "video/output/scene_04.mp4"
        render_scene_04(out, include_audio=not args.no_audio, cache_dir=args.cache_dir)
    elif args.scene == 5:
        out = args.output or "video/output/scene_05.mp4"
        render_scene_05(out, include_audio=not args.no_audio, cache_dir=args.cache_dir)
    else:
        out = args.output or "video/output/agentium_os.mp4"
        render_all(out, include_audio=not args.no_audio,
                   cache_dir_s1=args.cache_dir, cache_dir_s2=args.cache_dir_s2,
                   cache_dir_s3=args.cache_dir_s3, cache_dir_s4=args.cache_dir_s4,
                   cache_dir_s5=args.cache_dir_s5)


if __name__ == "__main__":
    main()
