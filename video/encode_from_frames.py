# SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
#
# SPDX-License-Identifier: Apache-2.0

"""
Quick encoder: render scenes (with frame cache) and produce output MP4s.

Usage:
    python video/encode_from_frames.py          # render both scenes → agentium_os.mp4
    python video/encode_from_frames.py --scene 1  # scene 1 only
    python video/encode_from_frames.py --scene 2  # scene 2 only
"""

from __future__ import annotations
import sys
import os
import argparse
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from video.render import (render_scene_01, render_scene_02, render_scene_03,
                           render_scene_04c, render_scene_04, render_scene_05, render_all)

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--scene", type=int, default=0,
                        help="0=all, 1-5=individual, 41=citations, 4=repository, 5=stack")
    args = parser.parse_args()

    os.makedirs("video/output", exist_ok=True)
    cache_s1  = "video/output/frame_cache_s1"
    cache_s2  = "video/output/frame_cache_s2"
    cache_s3  = "video/output/frame_cache_s3"
    cache_s4c = "video/output/frame_cache_s4c"
    cache_s4  = "video/output/frame_cache_s4"
    cache_s5  = "video/output/frame_cache_s5"

    if args.scene == 1:
        render_scene_01("video/output/scene_01.mp4", include_audio=True, cache_dir=cache_s1)
    elif args.scene == 2:
        render_scene_02("video/output/scene_02.mp4", include_audio=True, cache_dir=cache_s2)
    elif args.scene == 3:
        render_scene_03("video/output/scene_03.mp4", include_audio=True, cache_dir=cache_s3)
    elif args.scene == 41:
        render_scene_04c("video/output/scene_04c.mp4", include_audio=True, cache_dir=cache_s4c)
    elif args.scene == 4:
        render_scene_04("video/output/scene_04.mp4", include_audio=True, cache_dir=cache_s4)
    elif args.scene == 5:
        render_scene_05("video/output/scene_05.mp4", include_audio=True, cache_dir=cache_s5)
    else:
        render_all("video/output/agentium_os.mp4", include_audio=True,
                   cache_dir_s1=cache_s1, cache_dir_s2=cache_s2, cache_dir_s3=cache_s3,
                   cache_dir_s4c=cache_s4c, cache_dir_s4=cache_s4, cache_dir_s5=cache_s5)
