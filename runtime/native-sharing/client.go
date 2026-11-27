package main

import (
	"context"
	"crypto/ed25519"
	"crypto/tls"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"golang.org/x/term"
)

func tokenInput() (string, error) {
	fmt.Fprint(os.Stderr, "Sharing token: ")
	if term.IsTerminal(int(os.Stdin.Fd())) {
		bytes, err := term.ReadPassword(int(os.Stdin.Fd()))
		fmt.Fprintln(os.Stderr)
		return string(bytes), err
	}
	var token strings.Builder
	var byteIn [1]byte
	for token.Len() <= 128 {
		_, err := io.ReadFull(os.Stdin, byteIn[:])
		if errors.Is(err, io.EOF) || byteIn[0] == '\n' {
			break
		}
		if err != nil {
			return "", err
		}
		token.WriteByte(byteIn[0])
	}
	return token.String(), nil
}
func connect(ctx context.Context, dir, address string, saveToken, fresh bool, args []string) error {
	if _, _, err := net.SplitHostPort(address); err != nil {
		address = net.JoinHostPort(strings.Trim(address, "[]"), "7443")
	}
	known, err := readKnown(dir)
	if err != nil {
		return err
	}
	var token string
	var cred credential
	confirmed := known.Routes[address]
	if !fresh {
		token = known.Tokens[confirmed].Token
	}
	if token == "" && !fresh && confirmed == "" && len(known.Tokens) > 0 {
		raw, err := (&net.Dialer{Timeout: 8 * time.Second}).DialContext(ctx, "tcp", address)
		if err != nil {
			return err
		}
		probe := tls.Client(raw, &tls.Config{MinVersion: tls.VersionTLS13, NextProtos: []string{protocol}, InsecureSkipVerify: true, VerifyConnection: func(state tls.ConnectionState) error {
			if len(state.PeerCertificates) == 1 {
				if key, ok := state.PeerCertificates[0].PublicKey.(ed25519.PublicKey); ok {
					token = known.Tokens[encoding.EncodeToString(fingerprint(key))].Token
				}
			}
			return errors.New("identity lookup complete")
		}})
		probeCtx, cancel := context.WithTimeout(ctx, 8*time.Second)
		_ = probe.HandshakeContext(probeCtx)
		cancel()
		raw.Close()
	}
	if token == "" {
		token, err = tokenInput()
		if err != nil {
			return err
		}
	}
	cred, err = parseCredential(token)
	if err != nil {
		return err
	}
	if confirmed != "" && confirmed != encoding.EncodeToString(cred.pin) && !fresh {
		return errors.New("the computer at this address changed; use --pair to confirm its new token")
	}
	raw, err := (&net.Dialer{Timeout: 8 * time.Second}).DialContext(ctx, "tcp", address)
	if err != nil {
		return err
	}
	defer raw.Close()
	cfg := &tls.Config{MinVersion: tls.VersionTLS13, NextProtos: []string{protocol}, InsecureSkipVerify: true, VerifyConnection: cred.verify}
	tlsConn, err := authenticate(ctx, raw, cfg, false, cred.secret)
	if err != nil {
		return fmt.Errorf("sharing authentication failed (use --pair after a token reset): %w", err)
	}
	if err = tlsConn.SetReadDeadline(time.Now().Add(5 * time.Second)); err != nil {
		return err
	}
	info, err := readTarget(tlsConn)
	if err != nil {
		return err
	}
	if err = tlsConn.SetReadDeadline(time.Time{}); err != nil {
		return err
	}
	if saveToken {
		if err = remember(dir, address, token, info.Name, cred); err != nil {
			return err
		}
	}
	tmp, err := os.MkdirTemp("", "dim-link-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	endpoint := filepath.Join(tmp, "rpc.sock")
	if runtime.GOOS == "windows" {
		suffix, err := randomBytes(24)
		if err != nil {
			return err
		}
		endpoint = `\\.\pipe\dim-link-` + encoding.EncodeToString(suffix)
	}
	ready := filepath.Join(tmp, "ready")
	link, stop := context.WithCancel(ctx)
	done := make(chan error, 1)
	go func() { done <- runPeer(link, tlsConn, sibling("dim-terminal-transport"), "connect", endpoint, ready) }()
	defer func() { stop(); tlsConn.Close(); <-done }()
	deadline := time.NewTimer(15 * time.Second)
	defer deadline.Stop()
	tick := time.NewTicker(30 * time.Millisecond)
	defer tick.Stop()
	for {
		if _, err = os.Stat(ready); err == nil {
			break
		}
		select {
		case err := <-done:
			done <- err
			return fmt.Errorf("terminal transport unavailable: %v", err)
		case <-deadline.C:
			return errors.New("terminal transport startup timed out")
		case <-tick.C:
		}
	}
	cmd := exec.CommandContext(ctx, sibling("dimcode"), args...)
	cmd.Env = append(os.Environ(), "DIMCODE_TERMINAL_PROXY="+endpoint, "DIMCODE_TERMINAL_WORKSPACE="+info.WorkingDirectory)
	cmd.Stdin, cmd.Stdout, cmd.Stderr = os.Stdin, os.Stdout, os.Stderr
	return cmd.Run()
}
