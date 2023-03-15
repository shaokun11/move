from contextlib import ExitStack
import json
import os
import unittest
import tempfile
from collections import OrderedDict
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import (
    Any,
    Callable,
    Dict,
    Generator,
    List,
    Optional,
    Protocol,
    Sequence,
    Union,
)
from unittest.mock import patch

import forge
from forge import (
    ForgeCluster,
    ForgeConfigBackend,
    ForgeContext,
    ForgeFormatter,
    ForgeJob,
    ForgeResult,
    ForgeState,
    K8sForgeRunner,
    LocalForgeRunner,
    RunResult,
    SystemContext,
    ensure_provided_image_tags_has_profile_or_features,
    create_forge_command,
    find_recent_images,
    find_recent_images_by_profile_or_features,
    format_comment,
    format_pre_comment,
    format_report,
    get_all_forge_jobs,
    get_dashboard_link,
    get_humio_forge_link,
    get_humio_logs_link,
    get_testsuite_images,
    main,
    sanitize_forge_resource_name,
    validate_forge_config,
)

from click.testing import CliRunner, Result
from forge_wrapper_core.filesystem import Filesystem
from forge_wrapper_core.git import Git
from forge_wrapper_core.process import Process, Processes
from forge_wrapper_core.cluster import (
    GetPodsItem,
    GetPodsItemMetadata,
    GetPodsItemStatus,
    GetPodsResult,
    list_eks_clusters,
    AwsListClusterResult,
)

from forge_wrapper_core.shell import Shell
from forge_wrapper_core.time import Time

# Show the entire diff when unittest fails assertion
unittest.util._MAX_LENGTH = 2000  # type: ignore


class HasAssertMultiLineEqual(Protocol):
    def assertMultiLineEqual(self, first: str, second: str, msg: Any = ...) -> None:
        ...


def get_cwd() -> Path:
    return Path(__file__).absolute().parent


def get_fixture_path(fixture_name: str) -> Path:
    return get_cwd() / "fixtures" / fixture_name


class AssertFixtureMixin:
    def assertFixture(
        self: HasAssertMultiLineEqual, test_str: str, fixture_name: str
    ) -> None:
        fixture_path = get_fixture_path(fixture_name)
        if os.getenv("FORGE_WRITE_FIXTURES") == "true":
            print(f"Writing fixture to {str(fixture_path)}")
            fixture_path.write_text(test_str)
            fixture = test_str
        else:
            fixture = fixture_path.read_text()
        temp = Path(tempfile.mkstemp()[1])
        temp.write_text(test_str)
        self.assertMultiLineEqual(
            test_str,
            fixture,
            f"Fixture {fixture_name} does not match"
            "\n"
            f"Wrote to {str(temp)} for comparison"
            "\nRerun with FORGE_WRITE_FIXTURES=true to update the fixtures",
        )


class FakeShell(Shell):
    def run(self, command: Sequence[str], stream_output: bool = False) -> RunResult:
        return RunResult(0, b"output")

    async def gen_run(
        self, command: Sequence[str], stream_output: bool = False
    ) -> RunResult:
        return RunResult(0, b"async output")


class FakeFilesystem(Filesystem):
    def write(self, filename: str, contents: bytes) -> None:
        print(f"Wrote {contents} to {filename}")

    def read(self, filename: str) -> bytes:
        return b"fake"

    def mkstemp(self) -> str:
        return "temp"

    def rlimit(self, resource_type: int, soft: int, hard: int) -> None:
        return

    def unlink(self, filename: str) -> None:
        return


@dataclass
class FakeProcess(Process):
    _name: str
    _ppid: int

    def name(self) -> str:
        return self._name

    def ppid(self) -> int:
        return self._ppid


class FakeProcesses(Processes):
    def __init__(self) -> None:
        self.exit_callbacks = []

    def processes(self) -> Generator[Process, None, None]:
        yield FakeProcess("concensus", 1)

    def get_pid(self) -> int:
        return 2

    def spawn(self, target: Callable[[], None]) -> Process:
        return FakeProcess("child", 2)

    def atexit(self, callback: Callable[[], None]) -> None:
        return self.exit_callbacks.append(callback)

    def user(self) -> str:
        return "perry"


class FakeTime(Time):
    _now: int = 1659078000

    def now(self) -> datetime:
        return datetime.fromtimestamp(self._now, timezone.utc)

    def epoch(self) -> str:
        return str(self._now)


class FakeConfigBackend(ForgeConfigBackend):
    def __init__(self, store: object) -> None:
        self.store = store

    def create(self) -> None:
        pass

    def write(self, config: object) -> None:
        self.store = config

    def read(self) -> object:
        return self.store


class FakeCommand:
    def __init__(
        self, command: str, result_or_exception: Union[RunResult, Exception]
    ) -> None:
        self.command = command
        self.result_or_exception = result_or_exception

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, FakeCommand):
            return False
        return self.command == other.command

    def __hash__(self) -> int:
        return hash(self.command)

    def __repr__(self) -> str:
        return f"FakeCommand({self.command})"

    def __str__(self) -> str:
        return self.command


class SpyShell(FakeShell):
    def __init__(
        self,
        expected_command_list: Sequence[FakeCommand],
        strict: bool = False,
    ) -> None:
        self.expected_command_list = expected_command_list
        self.commands = []
        self.strict = strict

    def get_fake_commands(self) -> Sequence[str]:
        """Get the list of commands that are expected to be run"""
        return [fakecommand.command for fakecommand in self.expected_command_list]

    def run(self, command: Sequence[str], stream_output: bool = False) -> RunResult:
        """Mock a command run by adding it to a list of commands and returning the result"""
        rendered_command = " ".join(command)
        default = (
            Exception(f"Command not mocked: {rendered_command}")
            if self.strict
            else super().run(command)
        )
        # get how many times it's been called before, and use that to index into the expected command list
        # XXX: could be optimized, since it does N^2 scans of the command list
        times_called_before = self.commands.count(rendered_command)
        if rendered_command in self.get_fake_commands():
            try:
                command_index = [
                    i
                    for i, fakecommand in enumerate(self.expected_command_list)
                    if fakecommand.command == rendered_command
                ][times_called_before - 1]
            except IndexError:
                pretty_fake_cmds = "\n".join(self.get_fake_commands())
                raise Exception(
                    f"Did not find command {times_called_before} times in expected command list: {rendered_command}\n{pretty_fake_cmds}"
                )
            result = self.expected_command_list[command_index].result_or_exception
        else:
            raise Exception(
                f"Did not find command in expected command list: {rendered_command}"
            )
        self.commands.append(rendered_command)
        if isinstance(result, Exception):
            raise result
        return result

    async def gen_run(
        self, command: Sequence[str], stream_output: bool = False
    ) -> RunResult:
        return self.run(command, stream_output)

    def assert_commands(self, testcase) -> None:
        """Compare the list of commands that were run to the list of expected commands"""
        testcase.assertEqual(self.get_fake_commands(), self.commands)


class SpyFilesystem(FakeFilesystem):
    def __init__(
        self,
        expected_writes: Dict[str, bytes],
        expected_reads: Dict[str, bytes],
        expected_unlinks: Optional[List[str]] = None,
    ) -> None:
        self.expected_writes = expected_writes
        self.expected_reads = expected_reads
        self.expected_unlinks = expected_unlinks or []
        self.writes = {}
        self.reads = []
        self.temp_count = 1
        self.unlinks = []

    def write(self, filename: str, contents: bytes) -> None:
        self.writes[filename] = contents

    def get_write(self, filename: str) -> bytes:
        return self.writes[filename]

    def read(self, filename: str) -> bytes:
        self.reads.append(filename)
        return self.expected_reads.get(filename, b"")

    def assert_writes(self, testcase) -> None:
        for filename, contents in self.expected_writes.items():
            testcase.assertIn(
                filename, self.writes, f"{filename} was not written: {self.writes}"
            )
            testcase.assertMultiLineEqual(
                self.writes[filename].decode(),
                contents.decode(),
                f"{filename} did not match expected contents",
            )

    def assert_reads(self, testcase) -> None:
        for filename in self.expected_reads.keys():
            testcase.assertIn(filename, self.reads, f"{filename} was not read")

    def mkstemp(self) -> str:
        filename = f"temp{self.temp_count}"
        self.temp_count += 1
        return filename

    def unlink(self, filename: str) -> None:
        self.unlinks.append(filename)

    def assert_unlinks(self, testcase) -> None:
        for filename in self.expected_unlinks:
            testcase.assertIn(filename, self.unlinks, f"{filename} was not unlinked")


class SpyProcesses(FakeProcesses):
    def run_atexit(self) -> None:
        for callback in self.exit_callbacks:
            callback()


def fake_context(
    shell=None,
    filesystem=None,
    processes=None,
    time=None,
    mode=None,
) -> ForgeContext:
    return ForgeContext(
        shell=shell if shell else FakeShell(),
        filesystem=filesystem if filesystem else FakeFilesystem(),
        processes=processes if processes else FakeProcesses(),
        time=time if time else FakeTime(),
        forge_args=create_forge_command(
            forge_runner_mode=mode,
            forge_test_suite="banana",
            forge_runner_duration_secs="123",
            forge_num_validators="10",
            forge_num_validator_fullnodes="20",
            image_tag="asdf",
            upgrade_image_tag="upgrade_asdf",
            forge_namespace="forge-potato",
            forge_namespace_reuse="false",
            forge_namespace_keep="false",
            forge_enable_haproxy="false",
            cargo_args=["--cargo-arg"],
            forge_cli_args=["--forge-cli-arg"],
            test_args=["--test-arg"],
        ),
        aws_account_num="123",
        aws_region="banana-east-1",
        forge_image_tag="forge_asdf",
        image_tag="asdf",
        upgrade_image_tag="upgrade_asdf",
        forge_namespace="forge-potato",
        forge_cluster=ForgeCluster("tomato", "kubeconf"),
        forge_test_suite="banana",
        forge_blocking=True,
        github_actions="false",
        github_job_url="https://banana",
    )


class SpyTests(unittest.TestCase):
    def testSpyShell(self) -> None:
        shell = SpyShell(
            [
                FakeCommand(
                    "echo hello",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "echo hello_banana",
                    RunResult(0, b""),
                ),
            ]
        )
        shell.run(["echo", "hello"])
        shell.run(["echo", "hello_banana"])
        shell.assert_commands(self)


class ForgeRunnerTests(unittest.TestCase):
    maxDiff = None

    def testLocalRunner(self) -> None:
        cargo_run = " ".join(
            [
                "cargo",
                "run",
                "--cargo-arg",
                "-p",
                "aptos-forge-cli",
                "--",
                "--suite",
                "banana",
                "--duration-secs",
                "123",
                "--num-validators",
                "10",
                "--num-validator-fullnodes",
                "20",
                "--forge-cli-arg",
                "test",
                "k8s-swarm",
                "--image-tag",
                "asdf",
                "--upgrade-image-tag",
                "upgrade_asdf",
                "--namespace",
                "forge-potato",
                "--port-forward",
                "--test-arg",
            ]
        )
        shell = SpyShell(
            [
                FakeCommand(
                    cargo_run,
                    RunResult(0, b"orange"),
                ),
                FakeCommand(
                    "kubectl --kubeconfig kubeconf get pods -n forge-potato",
                    RunResult(0, b"Pods"),
                ),
            ]
        )
        filesystem = SpyFilesystem({}, {})
        context = fake_context(shell, filesystem, mode="local")
        runner = LocalForgeRunner()
        result = runner.run(context)
        self.assertEqual(result.state, ForgeState.PASS, result.output)
        shell.assert_commands(self)
        filesystem.assert_writes(self)
        filesystem.assert_reads(self)

    def testK8sRunner(self) -> None:
        self.maxDiff = None
        shell = SpyShell(
            [
                FakeCommand(
                    "kubectl --kubeconfig kubeconf delete pod -n default -l forge-namespace=forge-potato --force",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig kubeconf wait -n default --for=delete pod -l forge-namespace=forge-potato",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig kubeconf apply -n default -f temp1",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig kubeconf wait -n default --timeout=5m --for=condition=Ready pod/forge-potato-1659078000-asdf",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig kubeconf logs -n default -f forge-potato-1659078000-asdf",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig kubeconf get pod -n default forge-potato-1659078000-asdf -o jsonpath='{.status.phase}'",
                    RunResult(0, b"Succeeded"),
                ),
                FakeCommand(
                    "kubectl --kubeconfig kubeconf get pods -n forge-potato",
                    RunResult(0, b"Pods"),
                ),
            ]
        )
        forge_yaml = get_cwd() / "forge-test-runner-template.yaml"
        template_fixture = get_fixture_path("forge-test-runner-template.fixture")
        filesystem = SpyFilesystem(
            {
                "temp1": template_fixture.read_bytes(),
            },
            {
                "testsuite/forge-test-runner-template.yaml": forge_yaml.read_bytes(),
            },
        )
        context = fake_context(shell, filesystem, mode="k8s")
        runner = K8sForgeRunner()
        result = runner.run(context)
        shell.assert_commands(self)
        filesystem.assert_writes(self)
        filesystem.assert_reads(self)
        self.assertEqual(result.state, ForgeState.PASS, result.output)


class TestFindRecentImage(unittest.TestCase):
    def testFindRecentImage(self) -> None:
        shell = SpyShell(
            [
                FakeCommand("git rev-parse HEAD~0", RunResult(0, b"potato\n")),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator-testing --image-ids imageTag=potato",
                    RunResult(1, b""),
                ),
                FakeCommand("git rev-parse HEAD~1", RunResult(0, b"lychee\n")),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator-testing --image-ids imageTag=lychee",
                    RunResult(0, b""),
                ),
            ]
        )
        git = Git(shell)
        image_tags = find_recent_images(shell, git, 1, "aptos/validator-testing")
        self.assertEqual(list(image_tags), ["lychee"])
        shell.assert_commands(self)

    def testFindRecentFailpointsImage(self) -> None:
        shell = SpyShell(
            [
                FakeCommand("git rev-parse HEAD~0", RunResult(0, b"tomato\n")),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator-testing --image-ids imageTag=failpoints_tomato",
                    RunResult(0, b""),
                ),
            ]
        )
        git = Git(shell)
        image_tags = find_recent_images_by_profile_or_features(
            shell, git, 1, enable_performance_profile=False, enable_failpoints=True
        )
        self.assertEqual(list(image_tags), ["failpoints_tomato"])
        shell.assert_commands(self)

    def testFindRecentPerformanceImage(self) -> None:
        shell = SpyShell(
            [
                FakeCommand("git rev-parse HEAD~0", RunResult(0, b"potato\n")),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator-testing --image-ids imageTag=performance_potato",
                    RunResult(0, b""),
                ),
            ]
        )
        git = Git(shell)
        image_tags = find_recent_images_by_profile_or_features(
            shell,
            git,
            1,
            enable_performance_profile=True,
            enable_failpoints=False,
        )
        self.assertEqual(list(image_tags), ["performance_potato"])
        shell.assert_commands(self)

    def testFailBothFailpointsPerformance(self) -> None:
        shell = SpyShell([])
        git = Git(shell)
        with self.assertRaises(Exception):
            find_recent_images_by_profile_or_features(
                shell,
                git,
                1,
                enable_performance_profile=True,
                enable_failpoints=True,
            )

    def testDidntFindRecentImage(self) -> None:
        shell = SpyShell(
            [
                FakeCommand("git rev-parse HEAD~0", RunResult(0, b"crab\n")),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator-testing --image-ids imageTag=crab",
                    RunResult(1, b""),
                ),
            ]
        )
        git = Git(shell)
        with self.assertRaises(Exception):
            list(
                find_recent_images(
                    shell, git, 1, "aptos/validator-testing", commit_threshold=1
                )
            )

    def testFindRecentFewImages(
        self,
    ) -> None:  # such as in compat test where we find 2 images
        shell = SpyShell(
            [
                FakeCommand("git rev-parse HEAD~0", RunResult(0, b"crab\n")),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator --image-ids imageTag=crab",
                    RunResult(0, b""),
                ),
                FakeCommand("git rev-parse HEAD~1", RunResult(0, b"shrimp\n")),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator --image-ids imageTag=shrimp",
                    RunResult(0, b""),
                ),
            ]
        )
        git = Git(shell)
        images = find_recent_images(shell, git, 2, "aptos/validator")
        self.assertEqual(list(images), ["crab", "shrimp"])

    def testFailpointsProvidedImageTag(self) -> None:
        tag1, tag2 = ensure_provided_image_tags_has_profile_or_features(
            "potato_tomato",
            "failpoints_performance_potato",
            enable_failpoints=True,
            enable_performance_profile=False,
        )
        self.assertEqual(tag1, "failpoints_potato_tomato")  # it's added
        self.assertEqual(tag2, "failpoints_performance_potato")  # no change

    def testPerformaneProfilePartialProvidedImageTag(self) -> None:
        tag1, tag2 = ensure_provided_image_tags_has_profile_or_features(
            "potato_tomato",
            None,
            enable_failpoints=False,
            enable_performance_profile=True,
        )
        self.assertEqual(tag1, "performance_potato_tomato")  # it's added
        self.assertIsNone(tag2)

    def testFailpointsNoProvidedImageTag(self) -> None:
        tag1, tag2 = ensure_provided_image_tags_has_profile_or_features(
            None,
            None,
            enable_failpoints=True,
            enable_performance_profile=False,
        )
        self.assertIsNone(tag1)
        self.assertIsNone(tag2)


class ForgeFormattingTests(unittest.TestCase, AssertFixtureMixin):
    maxDiff = None

    def testTestsuiteImagesSameImage(self) -> None:
        context = fake_context()
        context.upgrade_image_tag = context.image_tag
        txt = get_testsuite_images(context)
        self.assertEqual(txt, f"`asdf`")

    def testTestsuiteImagesUpgrade(self) -> None:
        context = fake_context()
        txt = get_testsuite_images(context)
        self.assertEqual(
            txt,
            f"`asdf` ==> `upgrade_asdf`",
        )

    def testReport(self) -> None:
        filesystem = SpyFilesystem({"test": b"banana"}, {})
        context = fake_context(filesystem=filesystem)
        result = ForgeResult.from_args(ForgeState.PASS, "test")
        context.report(result, [ForgeFormatter("test", lambda c, r: "banana")])
        filesystem.assert_reads(self)
        filesystem.assert_writes(self)

    def testGetHumioLogsLinkRelative(self) -> None:
        link = get_humio_logs_link("forge-pr-2983", True)
        self.assertIn("forge-pr-2983", link)
        self.assertFixture(link, "testGetHumioLogsLinkRelative.fixture")

    def testGetHumioLogsLinkAbsolute(self) -> None:
        time = FakeTime()
        link = get_humio_logs_link("forge-pr-2984", (time.now(), time.now()))
        self.assertIn("forge-pr-2984", link)
        self.assertFixture(link, "testGetHumioLogsLinkAbsolute.fixture")

    def testGetHumioForgeLinkRelative(self) -> None:
        link = get_humio_forge_link("forge-pr-2985", True)
        self.assertIn("forge-pr-2985", link)
        self.assertFixture(link, "testGetHumioForgeLinkRelative.fixture")

    def testGetHumioForgeLinkAbsolute(self) -> None:
        link = get_humio_forge_link("forge-pr-2986", True)
        self.assertIn("forge-pr-2986", link)
        self.assertFixture(link, "testGetHumioForgeLinkAbsolute.fixture")

    def testDashboardLinkAutoRefresh(self) -> None:
        self.assertFixture(
            get_dashboard_link(
                "forge-pr-2983",
                "forge-big-1",
                True,
            ),
            "testDashboardLinkAutoRefresh.fixture",
        )

    def testDashboardLinkTimeInterval(self) -> None:
        self.assertFixture(
            get_dashboard_link(
                "forge-pr-2983",
                "forge-big-1",
                (
                    datetime.fromtimestamp(100000, timezone.utc),
                    datetime.fromtimestamp(100001, timezone.utc),
                ),
            ),
            "testDashboardLinkTimeInterval.fixture",
        )

    def testFormatPreComment(self) -> None:
        context = fake_context()
        pre_comment = format_pre_comment(context)
        self.maxDiff = 10
        self.assertIn(
            "var-namespace=forge-potato",
            pre_comment,
            "Wrong forge namespace in pre comment",
        )
        self.assertFixture(pre_comment, "testFormatPreComment.fixture")

    def testFormatComment(self) -> None:
        context = fake_context()
        report_fixture = get_fixture_path("report.fixture")
        with ForgeResult.with_context(context) as forge_result:
            forge_result.set_state(ForgeState.PASS)
            forge_result.set_output(report_fixture.read_text())
        forge_comment = format_comment(context, forge_result)
        self.assertIn(
            "var-namespace=forge-potato",
            forge_comment,
            "Wrong forge namespace in comment",
        )
        self.assertFixture(forge_comment, "testFormatComment.fixture")

    def testFormatReport(self) -> None:
        context = fake_context()
        report_fixture = get_fixture_path("report.fixture")
        with ForgeResult.with_context(context) as forge_result:
            forge_result.set_state(ForgeState.PASS)
            forge_result.set_output(report_fixture.read_text())
        self.assertFixture(
            format_report(context, forge_result),
            "testFormatReport.fixture",
        )

    def testSanitizeForgeNamespaceSlashes(self) -> None:
        namespace_with_slash = "forge-banana/apple"
        namespace = sanitize_forge_resource_name(namespace_with_slash)
        self.assertEqual(namespace, "forge-banana-apple")

    def testSanitizeForgeNamespaceTooLong(self) -> None:
        namespace_too_long = "forge-" + "a" * 10000
        namespace = sanitize_forge_resource_name(namespace_too_long)
        self.assertEqual(
            namespace,
            "forge-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )


class ForgeMainTests(unittest.TestCase, AssertFixtureMixin):
    maxDiff = None

    def testMain(self) -> None:
        runner = CliRunner()
        shell = SpyShell(
            [
                FakeCommand(
                    "aws sts get-caller-identity",
                    RunResult(0, b'{"Account": "123456789012"}'),
                ),
                FakeCommand(
                    # NOTE: with multi-cloud support, we set the kubeconfig to ensure auth before continuing
                    # See changes in: https://github.com/aptos-labs/aptos-core/pull/6166
                    "aws eks update-kubeconfig --name forge-big-1 --kubeconfig temp1",
                    RunResult(0, b""),
                ),
                FakeCommand("git rev-parse HEAD~0", RunResult(0, b"banana")),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator-testing "
                    "--image-ids imageTag=banana",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator-testing "
                    "--image-ids imageTag=banana",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/validator-testing "
                    "--image-ids imageTag=banana",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "aws ecr describe-images --repository-name aptos/forge --image-ids "
                    "imageTag=banana",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig temp1 delete pod -n default -l forge-namespace=forge-perry-1659078000 "
                    "--force",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig temp1 wait -n default --for=delete pod -l "
                    "forge-namespace=forge-perry-1659078000",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig temp1 apply -n default -f temp2",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig temp1 wait -n default --timeout=5m --for=condition=Ready "
                    "pod/forge-perry-1659078000-1659078000-banana",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig temp1 logs -n default -f forge-perry-1659078000-1659078000-banana",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig temp1 get pod -n default forge-perry-1659078000-1659078000-banana -o "
                    "jsonpath='{.status.phase}'",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl --kubeconfig temp1 get pods -n forge-perry-1659078000",
                    RunResult(0, b""),
                ),
            ]
        )
        filesystem = SpyFilesystem(
            {
                "temp-comment": get_fixture_path(
                    "testMainComment.fixture"
                ).read_bytes(),
                "temp-step-summary": get_fixture_path(
                    "testMainComment.fixture"
                ).read_bytes(),
                "temp-pre-comment": get_fixture_path(
                    "testMainPreComment.fixture"
                ).read_bytes(),
                "temp-report": get_fixture_path("testMainReport.fixture").read_bytes(),
            },
            {},
        )
        with ExitStack() as stack:
            stack.enter_context(runner.isolated_filesystem())
            stack.enter_context(
                patch.object(forge, "LocalFilesystem", lambda: filesystem)
            )
            stack.enter_context(patch.object(forge, "LocalShell", lambda *_: shell))
            stack.enter_context(patch.object(forge, "SystemTime", lambda: FakeTime()))
            stack.enter_context(
                patch.object(forge, "SystemProcesses", lambda: FakeProcesses())
            )
            stack.enter_context(
                patch.object(
                    forge,
                    "S3ForgeConfigBackend",
                    lambda *_: FakeConfigBackend(
                        {
                            "enabled_clusters": ["forge-big-1"],
                            "all_clusters": ["forge-big-1", "banana"],
                            "test_suites": {},
                        }
                    ),
                )
            )

            os.mkdir(".git")
            os.mkdir("testsuite")
            template_name = "forge-test-runner-template.yaml"
            Path(f"testsuite/{template_name}").write_text(
                (Path(__file__).parent / template_name).read_text()
            )
            result = runner.invoke(
                main,
                [
                    "test",
                    "--forge-cluster-name",
                    "forge-big-1",
                    "--forge-report",
                    "temp-report",
                    "--forge-pre-comment",
                    "temp-pre-comment",
                    "--forge-comment",
                    "temp-comment",
                    "--github-step-summary",
                    "temp-step-summary",
                    "--github-server-url",
                    "None",
                    "--github-repository",
                    "None",
                    "--github-run-id",
                    "None",
                    "banana-test",
                ],
                catch_exceptions=False,
            )
            shell.assert_commands(self)
            self.assertFixture(
                filesystem.get_write("temp-comment").decode(), "testMainComment.fixture"
            )
            self.assertFixture(
                filesystem.get_write("temp-step-summary").decode(),
                "testMainComment.fixture",
            )
            self.assertFixture(
                filesystem.get_write("temp-pre-comment").decode(),
                "testMainPreComment.fixture",
            )
            self.assertFixture(
                filesystem.get_write("temp-report").decode(), "testMainReport.fixture"
            )
            self.assertFixture(result.output, "testMain.fixture")


class TestListClusters(unittest.TestCase):
    def testListClusters(self) -> None:
        fake_clusters = json.dumps(
            AwsListClusterResult(
                clusters=[
                    "banana-fake-1",
                    "aptos-forge-banana-1",
                    "aptos-forge-potato-2",
                ]
            ),
        )
        shell = SpyShell(
            [
                FakeCommand(
                    "aws eks list-clusters", RunResult(0, fake_clusters.encode())
                ),
            ]
        )
        clusters = list_eks_clusters(shell)
        self.assertEqual(clusters, ["aptos-forge-banana-1", "aptos-forge-potato-2"])
        shell.assert_commands(self)

    def testListClustersFails(self) -> None:
        with self.assertRaises(Exception):
            shell = SpyShell(
                [
                    FakeCommand("Blah", RunResult(0, b"")),
                ]
            )
            list_eks_clusters(shell)
            shell.assert_commands(self)


def fake_pod_item(name: str, phase: str) -> GetPodsItem:
    return GetPodsItem(
        metadata=GetPodsItemMetadata(name=name), status=GetPodsItemStatus(phase=phase)
    )


class GetForgeJobsTests(unittest.IsolatedAsyncioTestCase):
    maxDiff = None

    async def testGetAllForgeJobs(self) -> None:
        fake_clusters = ["aptos-forge-banana", "aptos-forge-apple-2"]
        fake_first_pods = GetPodsResult(
            items=[
                fake_pod_item("forge-first", "Running"),
                fake_pod_item("forge-failed", "Failed"),
                fake_pod_item("ignore-me", "Failed"),
            ]
        )
        fake_second_pods = GetPodsResult(
            items=[
                fake_pod_item("forge-second", "Running"),
                fake_pod_item("forge-succeeded", "Succeeded"),
                fake_pod_item("me-too", "Failed"),
            ]
        )
        shell = SpyShell(
            [
                FakeCommand(
                    "aws eks update-kubeconfig --name aptos-forge-banana --kubeconfig temp1",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl get pods -n default --kubeconfig temp1 -o json",
                    RunResult(0, json.dumps(fake_first_pods).encode()),
                ),
                FakeCommand(
                    "aws eks update-kubeconfig --name aptos-forge-apple-2 --kubeconfig temp2",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "kubectl get pods -n default --kubeconfig temp2 -o json",
                    RunResult(0, json.dumps(fake_second_pods).encode()),
                ),
            ],
            strict=True,
        )
        filesystem = SpyFilesystem({}, {}, ["temp1", "temp2"])
        processes = SpyProcesses()
        context = SystemContext(shell, filesystem, processes, FakeTime())
        jobs = await get_all_forge_jobs(context, fake_clusters)
        expected_jobs = [
            ForgeJob(
                name="forge-first",
                phase="Running",
                cluster=ForgeCluster(
                    name="aptos-forge-banana",
                    kubeconf="temp1",
                ),
            ),
            ForgeJob(
                name="forge-failed",
                phase="Failed",
                cluster=ForgeCluster(
                    name="aptos-forge-banana",
                    kubeconf="temp1",
                ),
            ),
            ForgeJob(
                name="forge-second",
                phase="Running",
                cluster=ForgeCluster(
                    name="aptos-forge-apple-2",
                    kubeconf="temp2",
                ),
            ),
            ForgeJob(
                name="forge-succeeded",
                phase="Succeeded",
                cluster=ForgeCluster(
                    name="aptos-forge-apple-2",
                    kubeconf="temp2",
                ),
            ),
        ]
        self.assertEqual(jobs, expected_jobs)
        processes.run_atexit()
        filesystem.assert_unlinks(self)


class ForgeConfigTests(unittest.TestCase):
    maxDiff = None

    def testCreate(self) -> None:
        runner = CliRunner()
        shell = SpyShell(
            [
                FakeCommand("aws s3 mb s3://forge-wrapper-config", RunResult(0, b"")),
            ]
        )
        with patch.object(forge, "LocalShell", lambda: shell):
            result = runner.invoke(
                main,
                ["config", "create"],
                catch_exceptions=False,
            )
            shell.assert_commands(self)
            self.assertEqual(result.exit_code, 0)

    def testValidateInvalidConfig(self) -> None:
        self.assertEqual(
            validate_forge_config({}),
            [
                "Missing required field enabled_clusters",
                "Missing required field all_clusters",
            ],
        )

    def testValidateValidConfig(self) -> None:
        self.assertEqual(
            validate_forge_config(
                {
                    "enabled_clusters": ["banana"],
                    "all_clusters": ["banana", "apple"],
                }
            ),
            [],
        )

    def testValidateValidHelmConfig(self) -> None:
        self.assertEqual(
            validate_forge_config(
                {
                    "enabled_clusters": ["banana"],
                    "all_clusters": ["banana", "apple"],
                    "default_helm_values": {
                        "aptos-node": {"image": {"tag": "banana"}},
                        "aptos-genesis": {"image": {"tag": "banana"}},
                    },
                }
            ),
            [],
        )

    def testValidateInvalidHelmConfig(self) -> None:
        self.assertEqual(
            validate_forge_config(
                {
                    "enabled_clusters": ["banana"],
                    "all_clusters": ["banana", "apple"],
                    "default_helm_values": {
                        "apple": "enabled",
                        "banana": {"enabled": "true"},
                    },
                }
            ),
            [],
        )

    def testValidateMissingClusterConfig(self) -> None:
        self.assertEqual(
            validate_forge_config(
                {
                    "enabled_clusters": ["apple"],
                    "all_clusters": ["banana", "potato"],
                }
            ),
            [],
        )

    def testHelmGetConfig(self) -> None:
        helm_before = {
            "enabled_clusters": ["banana"],
            "all_clusters": ["banana", "apple"],
        }
        helm_after_missing = {
            "enabled_clusters": ["banana"],
            "all_clusters": ["banana", "apple"],
            "default_helm_values": {
                "aptos-node": {"apple": "enabled", "banana": {"enabled": "true"}}
            },
        }
        helm_after_complete = {
            "enabled_clusters": ["banana"],
            "all_clusters": ["banana", "apple"],
            "default_helm_values": {
                "aptos-node": {"apple": "enabled", "banana": {"enabled": "true"}},
                "aptos-genesis": {"apple": "enabled", "banana": {"enabled": "true"}},
            },
        }
        runner = CliRunner()
        shell = SpyShell(
            [
                FakeCommand(
                    "aws s3api get-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json temp1",
                    RunResult(0, json.dumps(helm_before).encode("utf-8")),
                ),
                FakeCommand(
                    "aws s3api get-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json temp2",
                    RunResult(0, json.dumps(helm_after_missing).encode("utf-8")),
                ),
                FakeCommand(
                    "aws s3api get-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json temp3",
                    RunResult(0, json.dumps(helm_after_complete).encode("utf-8")),
                ),
            ]
        )

        filesystem = SpyFilesystem(
            {},
            {
                "temp1": json.dumps(helm_before).encode(),
                "temp2": json.dumps(helm_after_missing).encode(),
                "temp3": json.dumps(helm_after_complete).encode(),
            },
        )
        with ExitStack() as stack:
            stack.enter_context(patch.object(forge, "LocalShell", lambda: shell))
            stack.enter_context(
                patch.object(forge, "LocalFilesystem", lambda: filesystem)
            )
            result_helm_config_not_present: Result = runner.invoke(
                main,
                ["config", "helm", "get", "aptos-node"],
                catch_exceptions=True,
            )
            result_helm_config_present_missing = runner.invoke(
                main,
                ["config", "helm", "get", "aptos-genesis"],
                catch_exceptions=True,
            )
            result_helm_config_present_complete = runner.invoke(
                main,
                ["config", "helm", "get", "aptos-node"],
                catch_exceptions=True,
            )
            # assert all commands and filesystem calls are correct
            shell.assert_commands(self)
            filesystem.assert_reads(self)
            filesystem.assert_writes(self)

            # assert that we error with a message when the config is not present
            self.assertEqual(result_helm_config_not_present.exit_code, 1)
            self.assertIsNotNone(result_helm_config_not_present.exception)
            self.assertEqual(
                result_helm_config_not_present.exception.args,  # type: ignore
                Exception("Missing key default_helm_values in Forge config").args,
            )

            # assert that we error with a message when the config is missing partial information
            self.assertEqual(result_helm_config_present_missing.exit_code, 1)
            self.assertIsNotNone(result_helm_config_present_missing.exception)
            self.assertEqual(
                result_helm_config_present_missing.exception.args,  # type: ignore
                Exception("No helm values found for chart aptos-genesis").args,
            )

            # we successfully get the config
            self.assertEqual(result_helm_config_present_complete.exit_code, 0)
            self.assertIsNotNone(helm_after_complete.get("default_helm_values"))
            self.assertIsNotNone(helm_after_complete.get("default_helm_values").get("aptos-node"))  # type: ignore
            # the output config is printed with an extra newline
            self.assertEqual(
                result_helm_config_present_complete.stdout_bytes,
                f'{json.dumps(helm_after_complete.get("default_helm_values").get("aptos-node"), indent=2)}\n'.encode(),  # type: ignore
            )

    def testHelmSetConfig(self) -> None:
        runner = CliRunner()
        shell = SpyShell(
            [
                FakeCommand(
                    "aws s3api get-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json temp1",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "aws s3api put-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json --body temp2",
                    RunResult(0, b""),
                ),
            ]
        )
        config_before = {
            "enabled_clusters": ["banana"],
            "all_clusters": ["banana", "apple"],
            "default_helm_values": {
                "aptos-node": {"apple": "enabled", "banana": {"enabled": "false"}}
            },
        }
        config_after = {
            **config_before,
            "default_helm_values": {
                "aptos-node": {"apple": "enabled", "banana": {"enabled": "true"}}
            },
        }
        filesystem = SpyFilesystem(
            {
                # new config which merges old config and new helm config written to temp file before pushing to s3
                "temp2": json.dumps(config_after).encode(),
            },
            {
                # read old config that has been written by s3 CLI
                "temp1": json.dumps(config_before).encode(),
                # read the new *helm* config from disk
                "temp2": json.dumps(
                    config_after["default_helm_values"]["aptos-node"]
                ).encode(),
            },
        )
        with ExitStack() as stack:
            stack.enter_context(patch.object(forge, "LocalShell", lambda: shell))
            stack.enter_context(
                patch.object(forge, "LocalFilesystem", lambda: filesystem)
            )
            ret = runner.invoke(
                main,
                ["config", "helm", "set", "aptos-node", "--config", "temp2", "-y"],
                catch_exceptions=True,
            )
            shell.assert_commands(self)
            filesystem.assert_reads(self)
            filesystem.assert_writes(self)

    def testHelmSetNewConfig(self) -> None:
        runner = CliRunner()
        shell = SpyShell(
            [
                FakeCommand(
                    "aws s3api get-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json temp1",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "aws s3api put-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json --body temp2",
                    RunResult(0, b""),
                ),
            ]
        )
        config_before = {
            "enabled_clusters": ["banana"],
            "all_clusters": ["banana", "apple"],
            "default_helm_values": {},
        }
        config_after = {
            **config_before,
            "default_helm_values": {
                "aptos-node": {"apple": "enabled", "banana": {"enabled": "true"}}
            },
        }
        filesystem = SpyFilesystem(
            {
                # new config which merges old config and new helm config written to temp file before pushing to s3
                "temp2": json.dumps(config_after).encode(),
            },
            {
                # read old config that has been written by s3 CLI
                "temp1": json.dumps(config_before).encode(),
                # read the new *helm* config from disk
                "temp2": json.dumps(
                    config_after["default_helm_values"]["aptos-node"]
                ).encode(),
            },
        )
        with ExitStack() as stack:
            stack.enter_context(patch.object(forge, "LocalShell", lambda: shell))
            stack.enter_context(
                patch.object(forge, "LocalFilesystem", lambda: filesystem)
            )
            ret = runner.invoke(
                main,
                ["config", "helm", "set", "aptos-node", "--config", "temp2", "-y"],
                catch_exceptions=True,
            )
            shell.assert_commands(self)
            filesystem.assert_reads(self)
            filesystem.assert_writes(self)

    def testHelmSetConfigPreview(self) -> None:
        runner = CliRunner()
        shell = SpyShell(
            [
                FakeCommand(
                    "aws s3api get-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json temp1",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "aws s3api put-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json --body temp2",
                    RunResult(0, b""),
                ),
            ]
        )
        config_fixture_before = get_fixture_path(
            "forge-default-helm-values-before.fixture"
        )
        config_fixture_after = get_fixture_path(
            "forge-default-helm-values-after.fixture"
        )
        config_applied = json.loads(config_fixture_after.read_bytes().decode())[
            "default_helm_values"
        ]["aptos-node"]
        config_fixture_preview = get_fixture_path(
            "forge-default-helm-values-preview.fixture"
        )
        filesystem = SpyFilesystem(
            {},
            {
                # read old config that has been written by s3 CLI
                "temp1": config_fixture_before.read_bytes(),
                # read the new *helm* config from disk
                "temp2": json.dumps(config_applied).encode(),
            },
        )
        with ExitStack() as stack:
            stack.enter_context(patch.object(forge, "LocalShell", lambda: shell))
            stack.enter_context(
                patch.object(forge, "LocalFilesystem", lambda: filesystem)
            )
            ret = runner.invoke(
                main,
                [
                    "config",
                    "helm",
                    "set",
                    "aptos-node",
                    "--config",
                    "temp2",
                    "-y",
                ],
                catch_exceptions=False,
            )
            shell.assert_commands(self)
            filesystem.assert_reads(self)
            filesystem.assert_writes(self)
            self.assertEqual(ret.exception, None)
            self.assertEqual(ret.exit_code, 0)
            assert ret.stdout_bytes.decode("utf-8").strip()
            self.assertEqual(
                ret.stdout_bytes.decode("utf-8").strip(),
                config_fixture_preview.read_bytes().decode("utf-8").strip(),
            )

    def testClusterDelete(self) -> None:
        runner = CliRunner()
        shell = SpyShell(
            [
                FakeCommand(
                    "aws s3api get-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json temp1",
                    RunResult(0, b""),
                ),
                FakeCommand(
                    "aws s3api put-object --bucket forge-wrapper-config --key "
                    "forge-wrapper-config.json --body temp2",
                    RunResult(0, b""),
                ),
            ]
        )
        clusters_before = {
            "enabled_clusters": ["banana"],
            "all_clusters": ["banana", "apple"],
        }
        clusters_after = {
            **clusters_before,
            "all_clusters": ["banana"],
        }
        filesystem = SpyFilesystem(
            {
                "temp2": json.dumps(clusters_after).encode(),
            },
            {
                "temp1": json.dumps(clusters_before).encode(),
            },
        )
        with ExitStack() as stack:
            stack.enter_context(patch.object(forge, "LocalShell", lambda: shell))
            stack.enter_context(
                patch.object(forge, "LocalFilesystem", lambda: filesystem)
            )
            runner.invoke(
                main,
                ["config", "cluster", "delete", "apple", "-y"],
                catch_exceptions=False,
            )
            shell.assert_commands(self)
            filesystem.assert_reads(self)
            filesystem.assert_writes(self)
