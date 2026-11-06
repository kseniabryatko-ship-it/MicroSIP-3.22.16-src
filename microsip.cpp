/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

 // microsip.cpp : Defines the class behaviors for the application.
 //
#include "stdafx.h"
#include "microsip.h"
#include "mainDlg.h"
#include "define.h"
#include "settings.h"
#include "langpack.h"

#include "Strsafe.h"

#include <Psapi.h>
#include <atomic>

#pragma warning(push)
#pragma warning(disable : 4091)
#include <Dbghelp.h>
#pragma warning(pop)

#include <sddl.h>

#include <commctrl.h>
#pragma comment(lib, "comctl32.lib")
#pragma comment(lib, "Psapi")
#pragma comment(lib, "Dbghelp")

#pragma comment(linker,"/manifestdependency:\"type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'\"")

#ifdef _DEBUG
#define new DEBUG_NEW
#endif

static HMODULE hUxtheme;

SetPreferredAppMode_t SetPreferredAppMode = nullptr;

// CmicrosipApp

BEGIN_MESSAGE_MAP(CmicrosipApp, CWinAppEx)
    ON_COMMAND(ID_HELP, &CWinAppEx::OnHelp)
END_MESSAGE_MAP()

void RecursiveDelete(CString path)
{
    if (!path.IsEmpty()) {
        CFileFind ff;
        if (path.Right(1) == _T("\\")) {
            path = path.Mid(0, path.GetLength() - 1);
        }
        BOOL res = ff.FindFile(path);
        while (res) {
            res = ff.FindNextFile();
            if (!ff.IsDots()) {
                if (ff.IsDirectory()) {
                    path = ff.GetFilePath();
                    CString path1 = path;
                    path1.Append(_T("\\*.*"));
                    RecursiveDelete(path1);
                    RemoveDirectory(path);
                }
                else {
                    DeleteFile(ff.GetFilePath());
                }
            }
        }
    }
}

bool EndsWith(const wchar_t* s, const wchar_t* suffix)
{
    if (!s || !suffix)
        return false;

    size_t len_s = wcslen(s);
    size_t len_suffix = wcslen(suffix);

    if (len_suffix > len_s)
        return false;

    return _wcsicmp(s + (len_s - len_suffix), suffix) == 0;
}

static bool IsModuleLoaded(const wchar_t* moduleName, bool part = false)
{
    HMODULE hMods[1024];
    DWORD cbNeeded;

    HANDLE hProcess = GetCurrentProcess();

    if (EnumProcessModules(hProcess, hMods, sizeof(hMods), &cbNeeded))
    {
        for (unsigned int i = 0; i < (cbNeeded / sizeof(HMODULE)); i++)
        {
            wchar_t szModName[MAX_PATH];

            if (GetModuleBaseNameW(hProcess, hMods[i], szModName,
                sizeof(szModName) / sizeof(wchar_t)))
            {
                if (part) {
                    if (_wcsnicmp(szModName, moduleName, wcslen(moduleName)) == 0)
                        return true;
                }
                else {
                    if (_wcsicmp(szModName, moduleName) == 0)
                        return true;
                }
            }
        }
    }
    return false;
}

// CmicrosipApp construction

CmicrosipApp::CmicrosipApp()
{
    // TODO: add construction code here,
    // Place all significant initialization in InitInstance
}


// The one and only CmicrosipApp object

CmicrosipApp theApp;

// CmicrosipApp initialization

CStringA wineVersion()
{
    using PWINE_GET_VERSION =
        const char* (CDECL*)(void);
    HMODULE hNtdll =
        GetModuleHandleW(L"ntdll.dll");
    if (hNtdll) {
        auto pWineGetVersion =
            reinterpret_cast<PWINE_GET_VERSION>(
                GetProcAddress(
                    hNtdll,
                    "wine_get_version"));
        if (pWineGetVersion) return pWineGetVersion();
    }
    return "n/a";
}

LONG WINAPI ExceptionFilter(EXCEPTION_POINTERS* ExceptionInfo)
{
    if (g_bClosingApp) {
        return EXCEPTION_EXECUTE_HANDLER;
    }

    bool disableCrashDump = false;
    CTime tm = CTime::GetCurrentTime();
    bool restart = (tm.GetTime() - startTime.GetTime()) > 10;
    CString message = Translate(_T("The application crashed"));
    CString descr = Translate(_T("This could be caused by driver issues, or other unexpected system problems."));
    wchar_t modulePath[MAX_PATH] = {};
    LPCTSTR moduleFileName = nullptr;
    DWORD moduleType = -1;
    void* faultAddress =
        ExceptionInfo->ExceptionRecord->ExceptionAddress;
    MEMORY_BASIC_INFORMATION mbi = {};
    if (VirtualQuery(faultAddress, &mbi, sizeof(mbi))) {
        moduleType = mbi.Type;
        HMODULE hModule = (HMODULE)mbi.AllocationBase;
        if (GetModuleFileName(hModule, modulePath, MAX_PATH)) {
            moduleFileName = PathFindFileName(modulePath);
        }
#ifdef _GLOBAL_VIDEO
        if (mbi.Type != MEM_IMAGE || !moduleFileName
            || _wcsicmp(moduleFileName, L"SDL2.dll") == 0
            || _wcsicmp(moduleFileName, L"RTWorkQ.DLL") == 0
            || StrStrIW(moduleFileName, L"video")
            || StrStrIW(moduleFileName, L"virtualcam")
            || StrStrIW(moduleFileName, L"camera"))
        {
            if (!g_bClosingApp) {
                if ((mainDlg && mainDlg->IsWindowVisible()) || !restart) {
                    if (moduleFileName || mbi.Type != MEM_IMAGE) {
                        message = Translate(_T("A critical error occurred in the video subsystem"));
                    }
                    if (moduleFileName) {
                        CString str = descr;
                        descr.Format(_T("%s\n\n%s"), modulePath, str);
                    }
                    descr.AppendFormat(_T("\n%s"), Translate(_T("Try the Lite version or update your graphics and camera drivers.")));
                    descr.AppendFormat(_T("\n\n%s"), Translate(_T("Open the download page now?")));
                    int nButtonPressed = 0;
                    TaskDialog(NULL, NULL,
                        _T(_GLOBAL_NAME_NICE),
                        message,
                        descr,
                        TDCBF_YES_BUTTON | TDCBF_NO_BUTTON,
                        TD_ERROR_ICON, &nButtonPressed);
                    if (nButtonPressed == IDYES) {
                        MSIP::OpenURL(_T(_GLOBAL_MENU_DOWNLOADS));
                    }
                }
            }
            disableCrashDump = true;
        }
        else
#endif
            if (_wcsicmp(moduleFileName, L"MMDevAPI.dll") == 0
                || _wcsicmp(moduleFileName, L"RTWorkQ.DLL") == 0
                ) {
                if (!g_bClosingApp) {
                    if ((mainDlg && mainDlg->IsWindowVisible()) || !restart) {
                        message = Translate(_T("A critical error occurred in the audio subsystem"));
                        TaskDialog(NULL, NULL,
                            _T(_GLOBAL_NAME_NICE),
                            message,
                            descr,
                            TDCBF_OK_BUTTON,
                            TD_ERROR_ICON, NULL);

                    }
                }
                disableCrashDump = true;
            }
            else if (EndsWith(moduleFileName, L".ax") || EndsWith(moduleFileName, L".drv")) {
                if (!g_bClosingApp) {
                    if ((mainDlg && mainDlg->IsWindowVisible()) || !restart) {
                        message = Translate(_T("A critical error occurred in the module on your PC"));
                        CString str = descr;
                        descr.Format(_T("%s\n\n%s"), modulePath, str);
                        TaskDialog(NULL, NULL,
                            _T(_GLOBAL_NAME_NICE),
                            message,
                            descr,
                            TDCBF_OK_BUTTON,
                            TD_ERROR_ICON, NULL);
                    }
                }
                disableCrashDump = true;
            }
    }
#ifdef _GLOBAL_VIDEO
    if (!disableCrashDump && (tm.GetTime() - videoUsedTime) <= 10) {
        if (!g_bClosingApp) {
            if ((mainDlg && mainDlg->IsWindowVisible()) || !restart) {
                descr.AppendFormat(_T("\n%s"), Translate(_T("Try the Lite version or update your graphics and camera drivers.")));
                descr.AppendFormat(_T("\n\n%s"), Translate(_T("Open the download page now?")));
                int nButtonPressed = 0;
                TaskDialog(NULL, NULL,
                    _T(_GLOBAL_NAME_NICE),
                    message,
                    descr,
                    TDCBF_YES_BUTTON | TDCBF_NO_BUTTON,
                    TD_ERROR_ICON, &nButtonPressed);
                if (nButtonPressed == IDYES) {
                    MSIP::OpenURL(_T(_GLOBAL_MENU_DOWNLOADS));
                }
            }
        }
        disableCrashDump = true;
    }
#endif
    if (!disableCrashDump) {
        CFile file;
        bool dmpCreated = false;
        CString filenameDmp;
        filenameDmp.Format(_T("%scrash-dump_%d.%d.%d.%d.dmp"), accountSettings.pathLocal, _GLOBAL_VERSION_COMMA);
        if (file.Open(filenameDmp, CFile::modeCreate | CFile::modeReadWrite)) {
            MINIDUMP_EXCEPTION_INFORMATION MinidumpExceptionInfo;
            MinidumpExceptionInfo.ThreadId = GetCurrentThreadId();
            MinidumpExceptionInfo.ExceptionPointers = ExceptionInfo;
            MinidumpExceptionInfo.ClientPointers = FALSE;
            MINIDUMP_TYPE dumpType = MiniDumpNormal;
            //if (!accountSettings.crashReport && accountSettings.enableLog) {
            //    dumpType = MiniDumpWithFullMemory;
            //}
            Json::Value root;
            statistics.toJson(root);
            root["crashTime"] = (int)tm.GetTime();
            root["cmdLine"] = MSIP::Utf8EncodeUni(theApp.m_lpCmdLine).GetString();
            root["portKnockerPorts"] = MSIP::Utf8EncodeUni(accountSettings.portKnockerPorts).GetString();
            root["portKnockerHost"] = MSIP::Utf8EncodeUni(accountSettings.portKnockerHost).GetString();
            if (moduleFileName) {
                root["module"] = MSIP::Utf8EncodeUni(moduleFileName).GetString();
            }
            root["moduleType"] = (int)moduleType;
            root["enableLog"] = accountSettings.enableLog;
            root["singleMode"] = accountSettings.singleMode;
            root["autoHangUpTime"] = accountSettings.autoHangUpTime;
            root["forceCodec"] = accountSettings.forceCodec;
            root["publicAddr"] = MSIP::Utf8EncodeUni(accountSettings.account.publicAddr).GetString();
            root["audioInputDevice"] = MSIP::Utf8EncodeUni(accountSettings.audioInputDevice).GetString();
            root["audioOutputDevice"] = MSIP::Utf8EncodeUni(accountSettings.audioOutputDevice).GetString();
            root["audioRingDevice"] = MSIP::Utf8EncodeUni(accountSettings.audioRingDevice).GetString();
            root["pjsua_state"] = pjsua_get_state();
            if (root["pjsua_state"] == PJSUA_STATE_RUNNING) {
                CStringA audDevNames;
                pjmedia_aud_dev_info aud_dev_info[PJMEDIA_AUD_MAX_DEVS];
                UINT count = PJMEDIA_AUD_MAX_DEVS;
                pjsua_enum_aud_devs(aud_dev_info, &count);
                for (unsigned i = 0; i < count; i++)
                {
                    audDevNames.AppendFormat("%s(%d,%d);", aud_dev_info[i].name, aud_dev_info[i].input_count, aud_dev_info[i].output_count);
                }
                root["audDevNames"] = audDevNames.GetString();
                root["pjsua_acc_count"] = pjsua_acc_get_count();
                root["pjsua_call_count"] = pjsua_call_get_count();
                root["pjsua_buddy_count"] = pjsua_get_buddy_count();
            }
            if (mainDlg && mainDlg->GetSafeHwnd()) {
                root["pDialer"] = (UINT)mainDlg->pageDialer;
                root["pCalls"] = (UINT)mainDlg->pageCalls;
                root["pContacts"] = (UINT)mainDlg->pageContacts;
                root["pTab"] = (UINT)&mainDlg->m_mainTab;
                root["pStatusBar"] = (UINT)&mainDlg->m_bar;
                root["pTabHWND"] = (UINT)mainDlg->m_mainTab.GetSafeHwnd();
                root["pTabDlgWND"] = mainDlg->GetDlgItem(IDC_MAIN_TAB) ? (UINT)mainDlg->GetDlgItem(IDC_MAIN_TAB)->GetSafeHwnd() : -1;
            }

            root["videoUsedTime"] = videoUsedTime;

            Json::FastWriter writer;
            std::string output = writer.write(root);
            MINIDUMP_USER_STREAM userStream;
            userStream.Type = 0x1000;
            userStream.BufferSize = output.size();
            userStream.Buffer = (PVOID)output.c_str();
            MINIDUMP_USER_STREAM_INFORMATION streamInfo;
            streamInfo.UserStreamCount = 1;
            streamInfo.UserStreamArray = &userStream;
            if (MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                file.m_hFile,
                dumpType,
                &MinidumpExceptionInfo,
                &streamInfo,
                nullptr
            )) {
                dmpCreated = true;
            }
            file.Close();
        }


        bool sent = false;
        CString blockFileName = accountSettings.pathLocal;
        blockFileName.AppendFormat(_T("block_%d.%d.%d.%d.dat"), _GLOBAL_VERSION_COMMA);
        bool blockDump = PathFileExists(blockFileName);
        CString filename;
        CStringA data;
        //---
        filename.Format(_T("%scrash-dump_%d.%d.%d.%d.txt"), accountSettings.pathLocal, _GLOBAL_VERSION_COMMA);
        if (file.Open(filename, CFile::modeCreate | CFile::modeWrite)) {
            data.AppendFormat("Time: %s (%lu)\r\n", CStringA(tm.FormatGmt(_T("%Y-%m-%dT%H:%M:%SZ"))), (int)tm.GetTime());
            data.AppendFormat("ExceptionCode: %lx\r\n", ExceptionInfo->ExceptionRecord->ExceptionCode);
            for (int i = 0; i < ExceptionInfo->ExceptionRecord->NumberParameters; i++) {
                data.AppendFormat("ExceptionInformation(%d): %lx\r\n", i, ExceptionInfo->ExceptionRecord->ExceptionInformation[i]);
            }
            if (moduleFileName) {
                data.AppendFormat("Module: %s\r\n", MSIP::Utf8EncodeUni(moduleFileName));
            }
            data.AppendFormat("Wine version: %s\r\n", wineVersion());
            //--
            DWORD dwVersion = 0;
            DWORD dwMajorVersion = 0;
            DWORD dwMinorVersion = 0;
            DWORD dwBuild = 0;
            dwVersion = GetVersion();
            dwMajorVersion = (DWORD)(LOBYTE(LOWORD(dwVersion)));
            dwMinorVersion = (DWORD)(HIBYTE(LOWORD(dwVersion)));
            if (dwVersion < 0x80000000) {
                dwBuild = (DWORD)(HIWORD(dwVersion));
            }
            CStringA winVersion;
            BOOL Wow64Process = FALSE;
            IsWow64Process(GetCurrentProcess(), &Wow64Process);
            winVersion.Format("%d.%d (%d) %s-bit",
                dwMajorVersion,
                dwMinorVersion,
                dwBuild,
                Wow64Process ? "32" : "64"
            );
            data.AppendFormat("Windows version: %s\r\n", winVersion);
            //--
            data.AppendFormat("Name: %s\r\nVersion: %d.%d.%d.%d\r\nState: %d\r\n",
                urlencode(_GLOBAL_NAME),
                _GLOBAL_VERSION_COMMA,
                pjsua_get_state()
            );
            //--
#ifdef _GLOBAL_VIDEO
            data.Append("Video: yes\r\n");
#else
            data.Append("Video: no\r\n");
#endif
            data.AppendFormat("Log enabled: %d\r\n", accountSettings.enableLog);
            if (accountSettings.enableLog && !accountSettings.logFile.IsEmpty()) {
                data.Append("\r\n");
                CStringA fileData;
                CStringA fileLine;
                try {
                    CFile file(accountSettings.logFile, CFile::modeRead | CFile::typeText | CFile::shareDenyNone);
                    UINT nBytes = (UINT)file.GetLength();
                    nBytes = file.Read(fileData.GetBuffer(nBytes + 1), nBytes);
                    fileData.ReleaseBuffer(nBytes);
                }
                catch (CFileException* pe)
                {
                    pe->Delete();
                }
                data.Append(fileData);
            }
            file.Write(data.GetString(), data.GetLength());
            file.Close();
        }
        //---
        bool sendCrashReport = false;
        bool txtSent = false;
        bool isEvasion = false;
        if (IsModuleLoaded(_T("EvasionPrevention.dll"))) {
            sendCrashReport = false;
            isEvasion = true;
        }
        if (sendCrashReport) {
            if (IsModuleLoaded(_T("SophosED.dll"))) {
                // Sophos
                sendCrashReport = false;
            }
            else if (IsModuleLoaded(_T("csxumd"), true)) {
                // CrowdStrike
                sendCrashReport = false;
            }
        }
        if (!blockDump) {
            CInternetSession session;
            try {
                CString readData;
                CHttpConnection* m_pHttp = session.GetHttpConnection(_T("crash-report2.microsip.org"));
                CHttpFile* pFile = m_pHttp->OpenRequest(CHttpConnection::HTTP_VERB_POST, _T("/crash-report?rev=2"));
                CString strHeaders = _T("Content-Type: application/x-www-form-urlencoded");
                CStringA strFormData;
                strFormData.Format("name=%s&version=%d.%d.%d.%d&state=%d",
                    urlencode(_GLOBAL_NAME),
                    _GLOBAL_VERSION_COMMA,
                    pjsua_get_state()
                );
#ifdef _GLOBAL_VIDEO
                strFormData.Append("&video=1");
#endif
                if (sendCrashReport) {
                    strFormData.AppendFormat("&dump=%s",
                        urlencode(data)
                    );
                }
                if (pFile->SendRequest(strHeaders, (LPVOID)strFormData.GetString(), strFormData.GetLength())) {
                    DWORD statusCode = 0;
                    pFile->QueryInfoStatusCode(statusCode);
                    if (statusCode == 200) {
                        txtSent = true;
                        pFile->ReadString(readData);
                    }
                    pFile->Close();
                }
                m_pHttp->Close();
                session.Close();
                if (readData == _T("stop")) {
                    CFile file;
                    if (file.Open(blockFileName, CFile::modeCreate)) {
                        file.Close();
                    }
                    blockDump = true;
                }
            }
            catch (CInternetException* e) {
            }
        }
        //---
        if (sendCrashReport && txtSent && !blockDump && dmpCreated) {
            if (file.Open(filenameDmp, CFile::modeRead)) {
                //---
                CInternetSession session;
                try {
                    CHttpConnection* m_pHttp = session.GetHttpConnection(_T("crash-report2.microsip.org"));
                    CHttpFile* pFile = m_pHttp->OpenRequest(CHttpConnection::HTTP_VERB_POST, _T("/crash-report"));
                    CString strHeaders = _T("Content-Type: application/octet-stream");
                    strHeaders.AppendFormat(_T("\r\nX-Name: %s\r\nX-Version: %d.%d.%d.%d\r\nX-State: %d"),
                        CString(urlencode(_GLOBAL_NAME_VISIBLE)),
                        _GLOBAL_VERSION_COMMA,
                        pjsua_get_state()
                    );
#ifdef _GLOBAL_VIDEO
                    strHeaders.Append(_T("\r\nX-Video: 1"));
#endif
                    pFile->AddRequestHeaders(strHeaders);
                    pFile->SendRequestEx(file.GetLength());
                    UINT len = 1;
                    char buf[1024];
                    file.SeekToBegin();
                    while (len) {
                        len = file.Read(buf, sizeof(buf));
                        if (len) {
                            pFile->Write(buf, len);
                        }
                    }
                    pFile->EndRequest();
                    pFile->Close();
                    m_pHttp->Close();
                    session.Close();
                    sent = true;
                }
                catch (CInternetException* e) {
                }
                //---
                file.Close();
            }
        }
        //---

        if (!g_bClosingApp) {
            if (blockDump) {
                descr = Translate(_T("Updating to the latest version may resolve this issue."));
                descr.AppendFormat(_T("\n\n%s"), Translate(_T("Open the download page now?")));
                int nButtonPressed = 0;
                TaskDialog(NULL, NULL,
                    _T(_GLOBAL_NAME_NICE),
                    message,
                    descr,
                    TDCBF_YES_BUTTON | TDCBF_NO_BUTTON,
                    TD_ERROR_ICON, &nButtonPressed);
                if (nButtonPressed == IDYES) {
                    MSIP::OpenURL(_T(_GLOBAL_MENU_DOWNLOADS));
                    return EXCEPTION_EXECUTE_HANDLER;
                }
            }
            else {
                if (!restart) {
                    if (isEvasion) {
                        descr = Translate(_T("The application may have been launched from an unsafe location. Please download the application from the official website."));
                        descr.AppendFormat(_T("\n\n%s"), Translate(_T("Open the download page now?")));
                        int nButtonPressed = 0;
                        TaskDialog(NULL, NULL,
                            _T(_GLOBAL_NAME_NICE),
                            message,
                            descr,
                            TDCBF_YES_BUTTON | TDCBF_NO_BUTTON,
                            TD_ERROR_ICON, &nButtonPressed);
                        if (nButtonPressed == IDYES) {
                            MSIP::OpenURL(_T(_GLOBAL_MENU_DOWNLOADS));
                        }
                    }
                    else {
                        if (sent) {
                            descr.Format(_T("Tracking info: %s\n\n"), tm.FormatGmt(_T("%Y-%m-%dT%H:%M:%SZ")));
                        }
                        descr.AppendFormat(_T(" %s"), Translate(_T("See the Freezes or Crashes help section.")));
                        descr.AppendFormat(_T("\n\n%s"), Translate(_T("Open troubleshooting page?")));
                        int nButtonPressed = 0;
                        TaskDialog(NULL, NULL,
                            _T(_GLOBAL_NAME_NICE),
                            message,
                            descr,
                            TDCBF_YES_BUTTON | TDCBF_NO_BUTTON,
                            TD_ERROR_ICON, &nButtonPressed);
                        if (nButtonPressed == IDYES) {
                            CString url = _T(_GLOBAL_MENU_ISSUES);
                            url.Append(_T("#crash"));
                            MSIP::OpenURL(url);
                        }
                    }
                }
            }
        }
    }
    if (restart && !g_bClosingApp) {
        // automatic restart after crash
        ShellExecute(NULL, NULL, accountSettings.exeFile, mainDlg && mainDlg->IsWindowVisible() ? NULL : _T("/minimized"), NULL, SW_SHOWDEFAULT);
    }
    return EXCEPTION_EXECUTE_HANDLER;
}

struct MsipEnumWindowsProcData {
    HINSTANCE hInst;
    HWND hWnd;
    int count;
};

BOOL CALLBACK MsipEnumWindowsProc(HWND hWnd, LPARAM lParam)
{
    MsipEnumWindowsProcData* data = (MsipEnumWindowsProcData*)lParam;
    HINSTANCE hInstance = (HINSTANCE)GetWindowLongPtr(hWnd, GWLP_HINSTANCE);
    if (hInstance && hInstance == data->hInst && GetWindow(hWnd, GW_OWNER) == (HWND)0) {
        TCHAR className[256];
        if (GetClassName(hWnd, className, 256)) {
            if (StrCmp(className, _T(_GLOBAL_NAME)) == 0) {
                //--
                DWORD dwProcessID;
                GetWindowThreadProcessId(hWnd, &dwProcessID);
                HANDLE hProcess = OpenProcess(PROCESS_QUERY_INFORMATION |
                    PROCESS_VM_READ, FALSE, dwProcessID);
                if (hProcess) {
                    TCHAR exeFilePath[MAX_PATH];
                    if (GetModuleFileNameEx(hProcess, NULL, exeFilePath, MAX_PATH)) {
                        if (StrCmpI(exeFilePath, accountSettings.exeFile) == 0) {
                            data->hWnd = hWnd;
                            data->count++;
                            return FALSE;
                        }
                    }
                    CloseHandle(hProcess);
                }
                //--
            }
        }
    }
    return TRUE;
}

BOOL CmicrosipApp::InitInstance()
{
    try {
        CString strCommandLine = theApp.m_lpCmdLine;
        accountSettings.Init();

        SetUnhandledExceptionFilter(ExceptionFilter);

        LoadLangPackModule();

        //WCHAR localeName[LOCALE_NAME_MAX_LENGTH + 1];
        //if (LCIDToLocaleName(langPack.localeID, localeName, LOCALE_NAME_MAX_LENGTH, 0))
        //{
        //    size_t len = wcslen(localeName);
        //    localeName[len + 1] = L'\0';
        //    PCZZWSTR pszLocales = localeName;
        //    ULONG numLanguages = 0;
        //    SetProcessPreferredUILanguages(
        //        MUI_LANGUAGE_NAME,
        //        pszLocales,
        //        &numLanguages);
        //    numLanguages++;
        //}

        MsipEnumWindowsProcData data;
        data.hInst = AfxGetInstanceHandle();
        data.count = 0;
        HWND hWndRunning = NULL;

            EnumWindows(MsipEnumWindowsProc, (LPARAM)&data);
            if (data.count) {
                hWndRunning = data.hWnd;
            }

        //*((char*)NULL) = 0; //produce a crash!!
        //SDL_DestroyWindow((SDL_Window*)545);

        bool cmdReset = lstrcmp(theApp.m_lpCmdLine, _T("/reset")) == 0;
        bool cmdResetNoAsk = lstrcmp(theApp.m_lpCmdLine, _T("/resetnoask")) == 0;
        if (cmdReset || cmdResetNoAsk) {
            if (hWndRunning) {
                ::SendMessage(hWndRunning, WM_CLOSE, NULL, NULL);
            }
            if (cmdResetNoAsk || AfxMessageBox(Translate(_T("Do you want to delete user data and program settings? This action cannot be undone.")), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_SYSTEMMODAL) == IDYES) {
                if (cmdResetNoAsk || AfxMessageBox(Translate(_T("Are you sure you want to delete?")), MB_YESNO | MB_ICONQUESTION | MB_SYSTEMMODAL) == IDYES) {
                    RecursiveDelete(accountSettings.appDataRoaming);
                    if (!::PathFileExists(accountSettings.appDataLocal + _T("Uninstall.exe"))) {
                        RecursiveDelete(accountSettings.appDataLocal);
                    }
                }
            }
            return FALSE;
        }
        bool cmdExit = lstrcmp(theApp.m_lpCmdLine, _T("/exit")) == 0;
        if (cmdExit) {
            if (hWndRunning) {
                ::SendMessage(hWndRunning, WM_COMMAND, MAKELPARAM(ID_EXIT, 0), NULL);
            }
            return FALSE;
        }
        if (hWndRunning) {
            if (lstrcmp(theApp.m_lpCmdLine, _T("/minimized")) == 0) {
            }
            else {
                bool activate = true;
                if (lstrlen(theApp.m_lpCmdLine)) {
                    COPYDATASTRUCT cd = {};
                    cd.dwData = 1;
                    cd.lpData = theApp.m_lpCmdLine;
                    cd.cbData = sizeof(TCHAR) * (lstrlen(theApp.m_lpCmdLine) + 1);
                    activate = ::SendMessage(hWndRunning, WM_COPYDATA, NULL, (LPARAM)&cd);
                }
                if (activate) {
                    ::ShowWindow(hWndRunning, SW_SHOW);
                    ::SetForegroundWindow(hWndRunning);
                }
            }
            return FALSE;
        }
        else {
            if (lstrcmp(theApp.m_lpCmdLine, _T("/answer")) == 0
                || lstrcmp(theApp.m_lpCmdLine, _T("/hangupall")) == 0
                ) {
                return FALSE;
            }
        }

        // InitCommonControlsEx() is required on Windows XP if an application
        // manifest specifies use of ComCtl32.dll version 6 or later to enable
        // visual styles.  Otherwise, any window creation will fail.
        // Set this to include all the common control classes you want to use
        // in your application.
        INITCOMMONCONTROLSEX InitCtrls;
        InitCtrls.dwSize = sizeof(InitCtrls);
        InitCtrls.dwICC = ICC_LISTVIEW_CLASSES |
            ICC_LINK_CLASS |
            ICC_BAR_CLASSES |
            ICC_LINK_CLASS |
            ICC_STANDARD_CLASSES |
            ICC_TAB_CLASSES |
            ICC_UPDOWN_CLASS;

        InitCommonControlsEx(&InitCtrls);

        // Initialize OLE libraries (need this for Virtual Display)
        if (!AfxOleInit())
        {
            AfxMessageBox(_T("OLE initialization failed. Make sure that the OLE libraries are the correct version."));
            return FALSE;
        }

        AfxEnableControlContainer();

        AfxInitRichEdit2();

        WSADATA wsaData;
        if (WSAStartup(MAKEWORD(2, 2), &wsaData) != 0) {
            AfxMessageBox(_T("WSAStartup failed. Please ensure the application has network access permissions."));
            return FALSE;
        }

        InitShellManager();

        WNDCLASS wc = {};
        // Get the info for this class.
        // #32770 is the default class name for dialogs boxes.
        if (!::GetClassInfo(NULL, L"#32770", &wc)) {
            AfxMessageBox(_T("GetClassInfo failed. Please contact the developer."));
            return FALSE;
        }
        wc.lpszClassName = _T(_GLOBAL_NAME);
        // Register this class so that MFC can use it.
        if (!::AfxRegisterClass(&wc)) {
            AfxMessageBox(_T("RegisterClass failed. Please contact the developer."));
            return FALSE;
        }

        hUxtheme = LoadLibraryEx(L"uxtheme.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
        if (hUxtheme) {
            auto fn1 = GetProcAddress(hUxtheme, MAKEINTRESOURCEA(134));
            auto fn2 = GetProcAddress(hUxtheme, "EndPanningFeedback"); // 10.0.22000+
            auto fn3 = GetProcAddress(hUxtheme, "EndBufferedAnimation"); // older win
            if (fn1) {
                if (fn1 == fn2 || fn1 == fn3) {
                    SetPreferredAppMode = reinterpret_cast<SetPreferredAppMode_t>(GetProcAddress(hUxtheme, MAKEINTRESOURCEA(135)));
                    if (SetPreferredAppMode) {
                        SetPreferredAppMode(AllowDark);
                    }
                }
            }
        }

        CTime t = CTime::GetCurrentTime();
        time_t time = t.GetTime();
        DWORD timeBuild = 0;
        int lifeTime1 = 365 * 1.5;
        int lifeTime2 = 365 * 2;
        int lifeTime3 = 365 * 2.5;
        CString message = Translate(_T("This version is no longer supported"));
        CString descr = Translate(_T("Please visit our website to download the latest version."));
        timeBuild = _GLOBAL_TIMESTAMP;
        if (time > timeBuild + 86400 * lifeTime3) {
            TaskDialog(NULL, AfxGetInstanceHandle(),
                _T(_GLOBAL_NAME_NICE),
                message,
                descr,
                TDCBF_OK_BUTTON,
                MAKEINTRESOURCE(IDI_INACTIVE), NULL);
            MSIP::OpenURL(_T(_GLOBAL_MENU_WEBSITE));
            exit(0);
        }
        else if (time > timeBuild + 86400 * lifeTime2) {
            notifyUpdate = 2;
        }
        else if (time > timeBuild + 86400 * lifeTime1) {
            notifyUpdate = 1;
        }

        CmainDlg* mainDlg = new CmainDlg;
        m_pMainWnd = mainDlg;

        if (!m_pMainWnd) {
            // return FALSE so that we exit the
            // application, rather than start the application's message pump.
            return FALSE;
        }

    }
    catch (std::exception& e)
    {
        AfxMessageBox(CA2T(e.what(), CP_UTF8), MB_ICONERROR);
        exit(EXIT_FAILURE);
    }
    return TRUE;
}

int CmicrosipApp::Run()
{
    try
    {
        return CWinAppEx::Run();
    }
    catch (const std::exception& e)
    {
        AfxMessageBox(CA2T(e.what(), CP_UTF8), MB_ICONERROR);
        return EXIT_FAILURE;
    }
}

int CmicrosipApp::ExitInstance()
{
    WSACleanup();
    return CWinAppEx::ExitInstance();
}
