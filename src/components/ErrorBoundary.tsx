import { Component, type ReactNode } from "react";

type Props = { children: ReactNode };
type State = { error: Error | null };

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  override render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 24, fontFamily: "monospace", color: "#c00", background: "#fff" }}>
          <b>React render error:</b>
          <pre style={{ whiteSpace: "pre-wrap", marginTop: 8 }}>
            {this.state.error.stack ?? this.state.error.message}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}
