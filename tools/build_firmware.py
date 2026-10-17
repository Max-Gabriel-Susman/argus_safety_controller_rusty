"""tools/build_firmware.py -- the Vitis GUI's Update XSA, Build platform,
Build app, as the Vitis Python API. Run through tools/build_firmware.sh,
which finds vitis and sources its environment:

    tools/build_firmware.sh

The workspace is this repo; the XSA is the sibling gateware repo's export,
or ARGUS_XSA. Refuses to run without the XSA rather than build a stale
platform. Exit status is the build's.
"""

import inspect
import os
import sys

import vitis  # provided by `vitis -s`

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), '..'))
XSA = os.environ.get(
    'ARGUS_XSA',
    os.path.join(os.path.dirname(REPO), 'argus-neural-codec', 'argus_neural_codec.xsa'))
PLATFORM = 'arty_z7_platform'
APP = 'safety_controller'


def update_hw(platform):
    """The GUI's Update XSA. The reference guide documents the keyword as
    `hw`; 2026.1 rejects it. Print what the API says about itself, then try
    the plausible forms, moving on only when the complaint is the keyword."""
    try:
        print(f'build_firmware: update_hw signature: {inspect.signature(platform.update_hw)}')
    except (TypeError, ValueError):
        pass
    doc = (getattr(platform.update_hw, '__doc__', None) or '').strip().splitlines()
    for line in doc[:8]:
        print(f'build_firmware:   doc: {line.strip()}')

    attempts = [{'hw_design': XSA}, {'hw': XSA}, {'hw_spec': XSA}, {'xsa': XSA}, {'hw_design_path': XSA}]
    for kw in attempts:
        name = next(iter(kw))
        try:
            platform.update_hw(**kw)
            print(f'build_firmware: update_hw({name}=...) accepted')
            return
        except TypeError as e:
            if 'unexpected keyword' not in str(e):
                raise
            print(f'build_firmware: update_hw({name}=...) rejected: {e}')
    try:
        platform.update_hw(XSA)
        print('build_firmware: update_hw(<positional>) accepted')
        return
    except TypeError as e:
        if 'positional' not in str(e) and 'argument' not in str(e):
            raise
        print(f'build_firmware: positional rejected: {e}')
    raise SystemExit('build_firmware: no accepted form of update_hw -- see the signature and docs above')


def main():
    if not os.path.isfile(XSA):
        print(f'build_firmware: no XSA at {XSA} -- run the gateware build first')
        return 2
    print(f'build_firmware: workspace {REPO}')
    print(f'build_firmware: xsa       {XSA}')

    client = vitis.create_client()
    try:
        client.set_workspace(path=REPO)

        platform = client.get_component(name=PLATFORM)
        print('build_firmware: updating platform hardware from the XSA')
        update_hw(platform)
        print('build_firmware: building platform')
        status = platform.build()
        print(f'build_firmware: platform build -> {status}')

        app = client.get_component(name=APP)
        print('build_firmware: building app')
        try:
            status = app.build()
        except TypeError:
            status = app.build(target='hw')
        print(f'build_firmware: app build -> {status}')
    finally:
        vitis.dispose()
    return 0


if __name__ == '__main__':
    sys.exit(main())
