"""Export the highest contiguous strict mip range from a 2D TTPF texture."""

import argparse
from pathlib import Path
import sys

# Isolated Python does not add the directory of the packaged decoder to sys.path.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import bcpack
import ttpf_mips


def read_dds(pool, index):
    if not 0 <= index < pool.textures:
        raise ValueError(f"Texture index {index} is outside this pool.")
    analysis = ttpf_mips.analyze(pool, index)
    if analysis.get("shape") != "2d" or "available" not in analysis:
        raise ValueError(analysis.get("blocked", "2D texture layout unavailable."))
    descriptor = analysis["descriptor"]
    if descriptor["format"] not in ttpf_mips.BC_FORMATS:
        raise ValueError(f"DXGI format {descriptor['format']} is not block compressed.")
    layout = analysis["layout"]
    cache = {}
    payload = bytearray()
    base_mip = count = top_size = 0
    reason = "No readable mips."
    for level, mip in enumerate(layout["mips"]):
        try:
            tiled = pool.read_range(
                descriptor["base"] + mip["offset"], mip["region"], cache, bcpack_mode="strict"
            )
        except (bcpack.BcpackProvisionalError, ttpf_mips.BcpackRequired) as error:
            if count:
                break
            reason = str(error)
            continue
        linear = ttpf_mips._visible_rows(layout, mip, ttpf_mips.detile(layout, mip, tiled))
        if 148 + len(payload) + len(linear) > ttpf_mips.MAX_OUTPUT:
            raise ValueError("DDS exceeds the 256 MiB output limit.")
        if count == 0:
            base_mip, top_size = level, len(linear)
        payload.extend(linear)
        count += 1
    if count == 0:
        raise ValueError(reason)
    return ttpf_mips.dds_bytes(
        max(1, descriptor["width"] >> base_mip), max(1, descriptor["height"] >> base_mip),
        descriptor["format"], count, top_size, payload
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pool", type=Path)
    parser.add_argument("index", type=int)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    try:
        pool = ttpf_mips.Pool(args.pool)
        try:
            dds = read_dds(pool, args.index)
        finally:
            pool.close()
        with args.output.open("xb") as output:
            output.write(dds)
    except (ValueError, OSError, NotImplementedError) as error:
        print(error, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
