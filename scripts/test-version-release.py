"""Offline release integration tests; no compilation or real publishing."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


def fake_gh(args):
    state_path = Path(os.environ['FAKE_GH_STATE'])
    state = json.loads(state_path.read_text(encoding='utf-8'))
    state['calls'].append(args)
    if args[0] == 'api' and '/releases/tags/' in args[1]:
        assert args[-2:] == ['--jq', '[.id, .draft] | @tsv']
        print('7\t' + str(state['draft']).lower())
    elif args[:2] == ['api', '--paginate']:
        assert args[-2:] == ['--jq', '.[] | [.name, .state, .size, .digest] | @tsv']
        for asset in state['assets']:
            print('\t'.join(str(asset[key]) for key in ['name', 'state', 'size', 'digest']))
    elif args[:2] == ['release', 'upload']:
        assert len(args) == 4, 'Uploads must never use --clobber'
        asset = Path(args[3])
        state['assets'].append(asset_record(asset))
    else:
        raise AssertionError('Unexpected gh invocation')
    state_path.write_text(json.dumps(state), encoding='utf-8')


def asset_record(path):
    data = path.read_bytes()
    return dict(name=path.name, state='uploaded', size=len(data),
                digest='sha256:' + hashlib.sha256(data).hexdigest())


class ReleaseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.shell = os.environ.get('TEST_SHELL') or shutil.which('sh')
        if not cls.shell and os.name == 'nt' and shutil.which('git'):
            cls.shell = str(Path(shutil.which('git')).parent.parent / 'usr/bin/bash.exe')
        if not cls.shell:
            raise RuntimeError('Install a POSIX shell or set TEST_SHELL')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='maa-release-tests-')
        self.work = Path(self.temp.name).resolve()
        self.addCleanup(self.cleanup)
        for name in ['.version-settings', 'package.json', 'package-lock.json',
                     'src-tauri/Cargo.toml', 'src-tauri/tauri.conf.json']:
            target = self.work / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)
        for name in ['ci', 'scripts/version-publish']:
            shutil.copytree(ROOT / name, self.work / name)
        self.version = json.loads((self.work / 'package.json').read_text(encoding="utf-8"))['version']
        self.branch = 'Release-' + self.version
        self.bin = self.work / 'test-bin'
        self.bin.mkdir()
        self.scratch = self.work / 'scratch'
        self.scratch.mkdir()
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(('RELEASE_', 'GITHUB_', 'GH_', 'VP_', 'XIL_', 'BASH_FUNC_'))
                    and k not in ('BASH_ENV', 'ENV')}
        self.env.update(PATH=str(self.bin) + os.pathsep + str(Path(self.shell).parent) + os.pathsep + os.environ['PATH'],
                        TMPDIR=self.scratch.as_posix(),
                        TEST_PYTHON=Path(sys.executable).as_posix(),
                        TEST_RUNNER=Path(__file__).resolve().as_posix())
        self.write('test-bin/curl', '#!/bin/sh\nprintf blocked > network-attempt\nexit 97\n')
        self.write('test-bin/gh', '#!/bin/sh\nexec "$TEST_PYTHON" "$TEST_RUNNER" --fake-gh "$@"\n')
        (self.bin / 'curl').chmod(0o755)
        (self.bin / 'gh').chmod(0o755)

    def cleanup(self):
        # Only remove the freshly created, resolved system-temp directory.
        assert self.work.parent == Path(tempfile.gettempdir()).resolve()
        assert self.work.name.startswith('maa-release-tests-')
        self.temp.cleanup()

    def write(self, name, content):
        (self.work / name).write_text(content, encoding='utf-8', newline='\n')

    def run_sh(self, script, *args, expected=0, env=None):
        probe = subprocess.run([self.shell, '-c', 'command -v curl'], env=env or self.env,
                               capture_output=True, text=True, cwd=self.work, timeout=10)
        self.assertIn('maa-release-tests-', probe.stdout, 'Network blocker must take precedence')
        result = subprocess.run([self.shell, script, *args], cwd=self.work, env=env or self.env,
                                stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                encoding='utf-8', timeout=30,
                                creationflags=0x08000000 if os.name == 'nt' else 0)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        self.assertFalse((self.work / 'network-attempt').exists(), 'Real publishing must never run')
        self.assertEqual(list(self.scratch.iterdir()), [], 'Temporary files must be cleaned')
        return result

    def gate(self, branch=None, expected=0, env=None):
        return self.run_sh('scripts/version-publish/check-version.sh', '--branch',
                           branch or self.branch, expected=expected, env=env)

    def test_current_version_and_two_platforms(self):
        self.assertEqual(self.run_sh('ci/read-version.sh').stdout.strip(), self.version)
        self.assertIn('platforms=2', self.gate().stdout)
        self.assertFalse((self.work / 'release-out').exists())

    def test_branch_case_canonical_form_and_version(self):
        for branch in ['release-' + self.version, 'Release-01.3.0', 'Release-100.0.0',
                       'Release-1.3.0-beta', 'Release-0.0.0', 'master']:
            with self.subTest(branch=branch):
                self.gate(branch, expected=2)

    def test_every_manual_version_companion_is_checked(self):
        for name, pointer in [('package.json', ['version']), ('package-lock.json', ['version']),
                              ('package-lock.json', ['packages', '', 'version'])]:
            with self.subTest(name=name, pointer=pointer):
                original = (self.work / name).read_text(encoding="utf-8")
                data = json.loads(original)
                target = data
                for key in pointer[:-1]:
                    target = target[key]
                target[pointer[-1]] = '0.0.0'
                self.write(name, json.dumps(data))
                self.gate(expected=2)
                self.write(name, original)
        name = 'src-tauri/Cargo.toml'
        self.write(name, (self.work / name).read_text(encoding="utf-8").replace(
            'version = "' + self.version + '"', 'version = "0.0.0"', 1))
        self.gate(expected=2)

    def test_missing_settings_requires_confirmation(self):
        (self.work / '.version-settings').unlink()
        self.assertIn('E_SETTINGS_MISSING_CONFIRMATION_REQUIRED', self.gate(expected=3).stderr)

    def test_settings_are_data_and_reject_secrets_duplicates_and_unknown_platforms(self):
        original = (self.work / '.version-settings').read_text(encoding="utf-8")
        cases = [original + 'GITHUB_REGISTRY_TOKEN=fake\n', original + 'RELEASE_BACKEND=github\n',
                 original.replace('windows-x64 macos-arm64', 'linux-x64'),
                 original.replace('GITHUB_REGISTRY_BRANCH=main', 'GITHUB_REGISTRY_BRANCH=$(touch injected)')]
        for text in cases:
            self.write('.version-settings', text)
            self.gate(expected=2)
            self.assertFalse((self.work / 'injected').exists())

    def test_ambient_configuration_cannot_override_settings(self):
        env = dict(self.env, RELEASE_PROJECT_CODE='wrong', GITHUB_RELEASE_TAG='wrong',
                   GITHUB_RELEASE_ASSET_NAME='wrong', GITHUB_REGISTRY_BRANCH='bad..branch')
        self.assertIn('project=my-ai-assistant', self.gate(env=env).stdout)

    def prepare(self):
        self.write('notes.txt', '版本发布测试\n')
        self.run_sh('ci/register-release.sh', 'prepare', self.branch, 'notes.txt')

    def test_prepare_both_platforms_offline(self):
        self.prepare()
        for platform in ['windows-x64', 'macos-arm64']:
            body = json.loads((self.work / 'release-out' / platform / 'release.json').read_text(encoding="utf-8"))
            manifest = f'https://github.com/bgluminous/my-ai-assistant/releases/download/v{self.version}/updater-{platform}.json'
            self.assertEqual(body, dict(version=self.version, changelog='版本发布测试\n', forceUpdate=False,
                                       extra={'updater': {'manifestUrl': manifest}}))

    def test_publish_rejects_missing_or_wrong_saved_input(self):
        self.run_sh('ci/register-release.sh', 'publish', self.branch, 'windows-x64', expected=2)
        self.prepare()
        self.write('release-out/windows-x64/release.json', '{"version":"0.0.0"}\n')
        result = self.run_sh('ci/register-release.sh', 'publish', self.branch, 'windows-x64', expected=2)
        self.assertIn('E_PREPARED_VERSION_MISMATCH', result.stderr)

    def test_registration_uses_saved_input_and_platform_specific_existing_asset(self):
        self.prepare()
        self.write('notes.txt', 'Changed after preparation\n')
        self.write('scripts/version-publish/publish-version.sh', '''#!/bin/sh
if [ "$1" = validate ]; then exit 0; fi
printf '%s\\n' "$GITHUB_RELEASE_TAG" "$GITHUB_RELEASE_ASSET_NAME" "$RELEASE_PLATFORM_CODE" "$@" > invocation
''')
        for platform, suffix in [('windows-x64', '_x64-setup.exe'), ('macos-arm64', '_aarch64.dmg')]:
            self.run_sh('ci/register-release.sh', 'publish', self.branch, platform)
            invocation = (self.work / 'invocation').read_text(encoding="utf-8").splitlines()
            self.assertEqual(invocation[:3], ['v' + self.version, 'my-ai-assistant_' + self.version + suffix, platform])
            self.assertNotIn('--asset', invocation)
            body = json.loads((self.work / 'release-out' / platform / 'release.json').read_text(encoding="utf-8"))
            self.assertEqual(body['changelog'], '版本发布测试\n')

    def upload_fixture(self, draft=True, existing=True):
        asset = self.work / 'app.exe'
        asset.write_bytes(b'original installer')
        state = dict(draft=draft, assets=[asset_record(asset)] if existing else [], calls=[])
        self.write('gh-state.json', json.dumps(state))
        self.env.update(GH_REPO='example/project', TAG='v' + self.version,
                        FAKE_GH_STATE=(self.work / 'gh-state.json').as_posix())
        return state

    def test_upload_missing_asset_and_verify_digest(self):
        self.upload_fixture(existing=False)
        self.run_sh('ci/upload-release-assets.sh', 'app.exe')
        state = json.loads((self.work / 'gh-state.json').read_text(encoding="utf-8"))
        self.assertEqual(sum(call[:2] == ['release', 'upload'] for call in state['calls']), 1)

    def test_matching_draft_asset_is_reused(self):
        self.upload_fixture()
        self.run_sh('ci/upload-release-assets.sh', 'app.exe')
        state = json.loads((self.work / 'gh-state.json').read_text(encoding="utf-8"))
        self.assertTrue(all(call[0] == 'api' for call in state['calls']))

    def test_conflicting_asset_is_never_replaced(self):
        state = self.upload_fixture()
        state['assets'][0]['digest'] = 'sha256:' + '0' * 64
        self.write('gh-state.json', json.dumps(state))
        result = self.run_sh('ci/upload-release-assets.sh', 'app.exe', expected=2)
        self.assertIn('E_ASSET_DIGEST_CONFLICT', result.stderr)
        state = json.loads((self.work / 'gh-state.json').read_text(encoding="utf-8"))
        self.assertTrue(all(call[0] == 'api' for call in state['calls']))

    def test_published_release_is_never_modified(self):
        self.upload_fixture(draft=False)
        self.run_sh('ci/upload-release-assets.sh', 'app.exe', expected=2)
        state = json.loads((self.work / 'gh-state.json').read_text(encoding="utf-8"))
        self.assertEqual(len(state['calls']), 1)


if __name__ == '__main__':
    if len(sys.argv) > 1 and sys.argv[1] == '--fake-gh':
        fake_gh(sys.argv[2:])
    else:
        unittest.main(verbosity=2)
