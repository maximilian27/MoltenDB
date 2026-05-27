# AI Usage & Transparency Declaration

MoltenDB's main objectives are to provide a secure, reliable, and efficient database/cache solution for developers and organizations while maintaining a strict level of privacy with no third-party dependencies. It was built with a heavy emphasis on Developer Experience (DX) and raw performance.

Coming from a frontend development background, the developer designed MoltenDB to be intuitive and user-friendly, leveraging GraphQL-style field projections and deeply nested filtering using a familiar JSON syntax. The core crate is specifically designed to be easily embeddable in other Rust applications, as well as simple to interface with other languages.

To accelerate the development process as a sole developer, Large Language Models (LLMs) were utilized. In accordance with open-source transparency standards, here is the strict breakdown of how AI is used in this repository.

## Human-Driven Architecture & Decisions
All architectural decisions are entirely human-made after careful consideration of the project's goals:
* **API & Query Design:** User experience, object chaining, and JSON syntax design.
* **Scope Reduction:** Strategic decisions to drop or defer certain functionalities (e.g., Bitcask scans, cold logs) in favor of efficiency. For a stable v1, MoltenDB is intentionally scoped as a single-node application.
* **Quality Assurance:** All performance, regression, and functional testing were executed manually to ensure product reliability.
* **UX/UI Design:** Core concepts, logo layouts, sample application structures, and color palettes were entirely conceptualized by the developer.
* **Crate Abstraction:** The deliberate decision to isolate the core crate to allow flexibility for future integrations, such as WebAssembly (WASM) and web packages.
* **Repository Segregation:** Moving the web packages architecture into a separate repository to cleanly adhere to modern web standards.
* **Storage Flexibility:** The architectural choice to support both persistent and ephemeral storage backends.

## AI-Assisted Implementation
The following models—including Gemini 3.x (Flash/Pro) and Claude 4.x (Sonnet/Opus) suites—were utilized to accelerate code execution:
* **Code Generation & Refactoring:** Drafting boilerplate, formatting, editing, and peer-reviewing code blocks.
* **Documentation & Changelogs:** Drafting, structure formatting, and editing documentation files.
* **Testing:** Writing foundational unit and integration test suites based on human code.
* **Asset Enhancement:** Polishing and optimizing the MoltenDB logo and UI assets from manual developer concepts.

LLM usage increases the productivity and efficiency of the developer, allowing more time to focus on core functionality, concepts, and system architecture.
