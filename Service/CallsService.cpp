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

#include "stdafx.h"
#include "CallsService.h"
#include "Markup.h"
#include "CSVFile.h"
#include "Utils/StringConverter.h"
#include "langpack.h"
#include "settings.h"

#include <fstream>
#include <string>
#include <iostream>

CallsService::CallsService(std::unique_ptr<ICallsRepository> repository)
    : m_repository(std::move(repository))
{
    auto calls = m_repository->GetAll(1);
    if (!calls.size()) {
        calls = IniCopyCalls();
    }
}

bool CallsService::Validate(const Call& call, std::string* error)
{
    if (call.callId.IsEmpty()) {
        if (error) *error = "Call ID is empty";
        return false;
    }
    else if (call.type < MSIP_CALL_TYPE_MIN || call.type > MSIP_CALL_TYPE_MAX) {
        if (error) *error = "Invalid call type: " + std::to_string(call.type);
        return false;
    }
    else if (call.name.IsEmpty()) {
        if (error) *error = "Name is empty";
        return false;
    }
    else if (call.number.IsEmpty()) {
        if (error) *error = "Phone number is empty";
        return false;
    }
    else if (call.time <= 0) {
        if (error) *error = "Invalid timestamp: " + std::to_string(call.time);
        return false;
    }
    else if (call.duration < 0) {
        if (error) *error = "Invalid duration: " + std::to_string(call.duration);
        return false;
    }
    if (error) *error = "";
    return true;
}

int CallsService::AddCall(Call& call)
{
    if (call.time == 0) {
        call.time = static_cast<long long>(time(nullptr));
    }
    if (!Validate(call)) {
        return 0;
    }

    int newId = m_repository->Add(call);
    call.id = newId;

    return newId;
}

void CallsService::UpdateCall(const Call& call)
{
    if (!Validate(call)) {
        return;
    }
    m_repository->Update(call);
}

void CallsService::DeleteCall(int id)
{
    m_repository->Delete(id);
}

void CallsService::DeleteCalls(std::vector<int> ids)
{
    m_repository->DeleteByIds(ids);
}

void CallsService::DeleteAllCalls()
{
    m_repository->DeleteAll();
}

std::unique_ptr<Call> CallsService::GetCall(int id)
{
    return m_repository->GetById(id);
}

std::unique_ptr<Call> CallsService::GetCallByCallId(const CString& callId)
{
    return m_repository->GetByCallId(callId);
}

std::vector<std::unique_ptr<Call>> CallsService::GetAllCalls()
{
    return m_repository->GetAll();
}

bool CallsService::IsCallFiletered(const Call& call, CString search) {
    if (!search.IsEmpty()) {
        search.MakeLower();
        CString name = call.name;
        CString number = call.number;
        name.MakeLower();
        number.MakeLower();
        if (name.Find(search) == -1 && number.Find(search) == -1) {
            return true;
        }
    }
    return false;
}


void CallsService::ExportCallsCSV(CString filename, CString search)
{
    CCSVFile CSVFile;
    CSVFile.SetCodePage(CP_UTF8);
    if (CSVFile.Open(filename, CCSVFile::modeCreate | CCSVFile::modeWrite | CFile::typeText | CFile::shareExclusive)) {
        CStringArray arr;
        arr.Add(_T("ID"));
        arr.Add(_T("Type"));
        arr.Add(_T("Name"));
        arr.Add(_T("Number"));
        arr.Add(_T("Timestamp"));
        arr.Add(_T("UTC Time"));
        arr.Add(_T("Local Time"));
        arr.Add(_T("Duration"));
        arr.Add(_T("Info"));
        CSVFile.WriteData(arr);
        auto calls = GetAllCalls();
        for (auto& pCall : calls) {
            if (IsCallFiletered(*pCall, search)) {
                continue;
            }
            arr.RemoveAll();
            arr.Add((pCall->callId));
            arr.Add(pCall->type == MSIP_CALL_TYPE_OUTGOING ? _T("outgoing") : (pCall->type == MSIP_CALL_TYPE_INCOMING ? _T("incoming") :
                (pCall->type == MSIP_CALL_TYPE_MISSED ? _T("missed") : _T("answered_elsewhere"))
                ));
            arr.Add((pCall->name));
            arr.Add((pCall->number));
            CString str;
            str.Format(_T("%d"), pCall->time);
            arr.Add(str);
            CTime t((time_t)pCall->time);
            str = t.FormatGmt(_T("%Y-%m-%dT%H:%M:%SZ"));
            arr.Add(str);
            str = t.Format(_T("%Y-%m-%d %H:%M:%S"));
            arr.Add(str);
            str.Format(_T("%d"), pCall->duration);
            arr.Add(str);
            arr.Add((pCall->info));
            CSVFile.WriteData(arr);
        }
        CSVFile.Close();
    }
}

std::string CallsService::ImportCallsCSV(CString filename)
{
    std::string result = "";
    CCSVFile CSVFile;
    CSVFile.SetCodePage(CP_UTF8);
    if (CSVFile.Open(filename, CCSVFile::modeRead | CFile::typeText | CFile::shareDenyWrite)) {
        CStringArray arr;
        int callIdIndex = -1;
        int typeIndex = -1;
        int nameIndex = -1;
        int numberIndex = -1;
        int timeIndex = -1;
        int durationIndex = -1;
        int infoIndex = -1;
        bool bHeader = true;

        m_repository->BeginTransaction();

        int nLine = 1;

        while (CSVFile.ReadData(arr)) {
            if (bHeader) {
                for (int i = 0; i < arr.GetCount(); i++) {
                    CString s = arr.GetAt(i);
                    if (callIdIndex == -1 && s == _T("ID")) {
                        callIdIndex = i;
                    }
                    else if (typeIndex == -1 && s == _T("Type")) {
                        typeIndex = i;
                    }
                    else if (nameIndex == -1 && s == _T("Name")) {
                        nameIndex = i;
                    }
                    else if (numberIndex == -1 && s == _T("Number")) {
                        numberIndex = i;
                    }
                    else if (timeIndex == -1 && s == _T("Timestamp")) {
                        timeIndex = i;
                    }
                    else if (durationIndex == -1 && s == _T("Duration")) {
                        durationIndex = i;
                    }
                    else if (infoIndex == -1 && s == _T("Info")) {
                        infoIndex = i;
                    }
                }
                if (callIdIndex == -1 ||
                    typeIndex == -1 ||
                    nameIndex == -1 ||
                    numberIndex == -1 ||
                    timeIndex == -1
                    ) {
                    result = "The received data cannot be recognized";
                    break;
                }
                bHeader = false;
            }
            else {
                Call call;
                if (callIdIndex != -1 && arr.GetCount() > callIdIndex) {
                    call.callId = (arr.GetAt(callIdIndex));
                }
                if (typeIndex != -1 && arr.GetCount() > typeIndex) {
                    call.type = _wtoi(arr.GetAt(typeIndex));
                }
                if (nameIndex != -1 && arr.GetCount() > nameIndex) {
                    call.name = (arr.GetAt(nameIndex));
                }
                if (numberIndex != -1 && arr.GetCount() > numberIndex) {
                    call.number = (arr.GetAt(numberIndex));
                }
                if (timeIndex != -1 && arr.GetCount() > timeIndex) {
                    call.time = _wtoi(arr.GetAt(timeIndex));
                }
                if (durationIndex != -1 && arr.GetCount() > durationIndex) {
                    call.duration = _wtoi(arr.GetAt(durationIndex));
                }
                if (infoIndex != -1 && arr.GetCount() > infoIndex) {
                    call.info = (arr.GetAt(infoIndex));
                }
                std::string err;
                if (Validate(call, &err)) {
                    if (!m_repository->GetByCallId(call.callId)) {
                        m_repository->Add(call);
                    }
                }
                else {
                    result = "Error in line " + std::to_string(nLine) + "\r\n\r\n" + err + "\r\n\r\n";
                    for (int i = 0; i < arr.GetCount(); i++) {
                        if (i != 0) result += ",";
                        result += CStringToUtf8(arr.GetAt(i));
                    }
                    break;
                }
            }
            nLine++;
        }
        if (result.empty()) {
            m_repository->Commit();
        }
        else {
            m_repository->Rollback();
        }
        CSVFile.Close();
    }
    return result;
}

void CallsService::IniCallDecode(CString str, Call& call)
{
    call.number = str;
    call.name = call.number;
    call.type = 0;
    call.time = 0;
    call.duration = 0;
    CString rab;
    int begin;
    int end;
    begin = 0;
    end = str.Find(';', begin);

    if (end != -1)
    {
        call.number = str.Mid(begin, end - begin);
        begin = end + 1;
        end = str.Find(';', begin);
        if (end != -1)
        {
            call.name = str.Mid(begin, end - begin);
            begin = end + 1;
            end = str.Find(';', begin);
            if (end != -1)
            {
                call.type = atoi(CStringA(str.Mid(begin, end - begin)));
                if (call.type > 3 || call.type < 0) {
                    call.type = 0;
                }
                begin = end + 1;
                end = str.Find(';', begin);
                if (end != -1)
                {
                    call.time = atoi(CStringA(str.Mid(begin, end - begin)));
                    begin = end + 1;
                    end = str.Find(';', begin);
                    if (end != -1)
                    {
                        call.duration = atoi(CStringA(str.Mid(begin, end - begin)));
                        begin = end + 1;
                        end = str.Find(';', begin);
                        if (end != -1)
                        {
                            call.info = str.Mid(begin, end - begin);
                            begin = end + 1;
                            end = str.Find(';', begin);
                        }
                        else {
                            call.info = str.Mid(begin);
                        }
                    }
                }
            }
        }
    }
}

std::vector<std::unique_ptr<Call>> CallsService::IniCopyCalls()
{
    std::vector<std::unique_ptr<Call>> calls;
    CString key;
    CString val;
    int prevTime = 0;
    int minTime = 0;
    int pos = -1;
    int inserted = 0;
    int i = 0;
    CString str;
    LPTSTR ptr;
    ptr = str.GetBuffer(255);
    while (true) {
        key.Format(_T("%d"), i);
        if (GetPrivateProfileString(_T("Calls"), key, NULL, ptr, 256, accountSettings.iniFile)) {
            if (str != _T("null")) {
                std::unique_ptr<Call> pCall = std::make_unique <Call>();
                IniCallDecode(ptr, *pCall);
                CString callId;
                callId.Format(_T("%d-%d"), i, pCall->time);
                pCall->callId = CString(msip_md5sum(CStringA(callId)));
                calls.push_back(std::move(pCall));
            }
        }
        else {
            break;
        }
        i++;
    }
    str.ReleaseBuffer();
    std::sort(calls.begin(), calls.end(),
        [](const std::unique_ptr<Call>& a, const std::unique_ptr<Call>& b)
        {
            return a->time < b->time;
        });
    for (auto& pCall : calls) {
        m_repository->Add(*pCall);
    }
    WritePrivateProfileSection(_T("Calls"), NULL, accountSettings.iniFile);
    WritePrivateProfileString(_T("Settings"), _T("callsLastKey"), NULL, accountSettings.iniFile);
    return calls;
}
