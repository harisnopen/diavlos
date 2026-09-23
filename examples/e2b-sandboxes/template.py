"""Build the `diavlos-agents` E2B template: a Python sandbox with the
`diavlos` CLI and its Python binding installed.

    pip install e2b
    E2B_API_KEY=... python template.py

Then `Sandbox.create("diavlos-agents")` gives you a sandbox where agents can
join a Diavlos room with nothing else to set up.
"""
import os

from e2b import Template

ALIAS = os.environ.get("TEMPLATE_ALIAS", "diavlos-agents")
BINDING = "git+https://github.com/harisnopen/diavlos#subdirectory=bindings/python"

template = (
    Template()
    # Debian 13: new enough (glibc 2.41) to run a diavlos built on a current Linux.
    .from_python_image("3.12-trixie")
    .apt_install(["curl", "ca-certificates", "git"])
    # The release installer checks the sigstore bundle when cosign is present.
    .run_cmd("curl -fsSL https://raw.githubusercontent.com/harisnopen/diavlos/main/install.sh "
             "| DIAVLOS_INSTALL_DIR=/usr/local/bin sh", user="root")
    .run_cmd(f'pip install "diavlos[verify] @ {BINDING}"', user="root")
    .run_cmd("diavlos --version")
)


if __name__ == "__main__":
    info = Template.build(template, name=ALIAS, cpu_count=1, memory_mb=1024,
                          on_build_logs=lambda e: print(e))
    print(f"built template {ALIAS}: {info}")
